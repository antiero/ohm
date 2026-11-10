//! C ABI (feature `worklet`) for hosting the engine in an AudioWorklet (or any wasm32 host).
//!
//! There is no wasm-bindgen: the host copies source text into memory it gets
//! from `ohm_alloc`, calls `ohm_compile`, then calls `ohm_render` once per
//! audio block. Compiling a new patch swaps it in while keeping the state of
//! the old one, exactly like the file watcher in `main.rs`.

use crate::{Limiter, Program, build, render};

const MAX_SOURCE_BYTES: usize = 64 * 1024;
/// Budgets so one patch cannot exhaust a shared audio thread or its memory.
const MAX_OPS: usize = 50_000;
const MAX_BUFFER_FLOATS: usize = 8 * 1024 * 1024;

pub struct Engine {
    program: Option<Program>,
    limiter: Limiter,
    sample_rate: u32,
    error: String,
    external_bpm: Option<f32>,
}

#[unsafe(no_mangle)]
pub extern "C" fn ohm_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// # Safety
/// `ptr` and `len` must come from a matching `ohm_alloc(len)` call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohm_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(unsafe { Vec::from_raw_parts(ptr, 0, len.max(1)) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ohm_engine_new(sample_rate: u32) -> *mut Engine {
    Box::into_raw(Box::new(Engine {
        program: None,
        limiter: Limiter::default(),
        sample_rate: sample_rate.max(8_000),
        error: String::new(),
        external_bpm: None,
    }))
}

/// # Safety
/// `engine` must come from `ohm_engine_new` and not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohm_engine_free(engine: *mut Engine) {
    if !engine.is_null() {
        drop(unsafe { Box::from_raw(engine) });
    }
}

unsafe fn text<'a>(ptr: *const u8, len: usize) -> &'a str {
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(ptr, len) }).unwrap_or("")
}

/// Compiles `src` (with `prelude` in front) and swaps it in. Returns 0 on
/// success. On failure the previous program keeps playing and the message is
/// available from `ohm_error_ptr` / `ohm_error_len`.
///
/// # Safety
/// Both pointers must reference `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohm_compile(
    engine: *mut Engine,
    prelude_ptr: *const u8,
    prelude_len: usize,
    src_ptr: *const u8,
    src_len: usize,
) -> i32 {
    let engine = unsafe { &mut *engine };
    engine.error.clear();
    if src_len > MAX_SOURCE_BYTES {
        engine.error = format!("patch is too long ({src_len} bytes, limit {MAX_SOURCE_BYTES})");
        return 2;
    }
    let (prelude, src) = unsafe { (text(prelude_ptr, prelude_len), text(src_ptr, src_len)) };
    match build(prelude, src) {
        Ok(mut next) => {
            if next.op_count() > MAX_OPS {
                engine.error = format!("patch is too heavy ({} operations, limit {MAX_OPS})", next.op_count());
                return 3;
            }
            if next.buffer_len() > MAX_BUFFER_FLOATS {
                engine.error = "patch uses too much delay memory".to_string();
                return 3;
            }
            if let Some(old) = &engine.program {
                next.copy_state_from(old);
            }
            next.set_external_bpm(engine.external_bpm);
            engine.program = Some(next);
            0
        }
        Err(error) => {
            engine.error = format!("{error:#}");
            1
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ohm_error_ptr(engine: *const Engine) -> *const u8 {
    unsafe { (&(*engine).error).as_ptr() }
}

#[unsafe(no_mangle)]
pub extern "C" fn ohm_error_len(engine: *const Engine) -> usize {
    unsafe { (&(*engine).error).len() }
}

/// Sets the host tempo, and optionally jumps the ambient clock to
/// `position_cycles` bars. A `bpm` defined by the patch wins over the host.
#[unsafe(no_mangle)]
pub extern "C" fn ohm_set_transport(engine: *mut Engine, bpm: f32, position_cycles: f64, set_position: i32) {
    let engine = unsafe { &mut *engine };
    engine.external_bpm = (bpm.is_finite() && bpm > 0.0).then_some(bpm);
    if let Some(program) = &mut engine.program {
        program.set_external_bpm(engine.external_bpm);
        if set_position != 0 {
            program.set_position(position_cycles);
        }
    }
}

/// 1 when the patch sets its own `bpm`, so the host tempo is ignored.
#[unsafe(no_mangle)]
pub extern "C" fn ohm_defines_bpm(engine: *const Engine) -> i32 {
    unsafe { (&(*engine).program).as_ref().is_some_and(|p| p.defines_bpm()) as i32 }
}

#[unsafe(no_mangle)]
pub extern "C" fn ohm_position(engine: *const Engine) -> f64 {
    unsafe { (&(*engine).program).as_ref().map_or(0.0, |p| p.position()) }
}

/// Renders `frames` stereo frames into two planar buffers.
///
/// # Safety
/// `left` and `right` must each have room for `frames` floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohm_render(engine: *mut Engine, left: *mut f32, right: *mut f32, frames: usize) {
    let engine = unsafe { &mut *engine };
    let (left, right) = unsafe {
        (
            std::slice::from_raw_parts_mut(left, frames),
            std::slice::from_raw_parts_mut(right, frames),
        )
    };
    let Some(program) = &mut engine.program else {
        left.fill(0.0);
        right.fill(0.0);
        return;
    };
    let mut frame = [0.0f32; 2];
    for (l, r) in left.iter_mut().zip(right.iter_mut()) {
        render(program, &mut frame, engine.sample_rate);
        engine.limiter.process(&mut frame, engine.sample_rate);
        *l = frame[0];
        *r = frame[1];
    }
}
