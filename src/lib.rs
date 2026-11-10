mod ast;
mod compiler;
mod env;
mod expand;
mod ir;
mod lexer;
mod limiter;
mod parser;
mod pitch;
mod stack;
mod vm;
// the author's local (untracked) wasm-bindgen layer; replaced by `worklet` when that feature is on
#[cfg(all(target_arch = "wasm32", not(feature = "worklet")))]
mod wasm;
// C ABI for hosting in an AudioWorklet: `cargo build --lib --release --target wasm32-unknown-unknown --features worklet`
#[cfg(feature = "worklet")]
mod worklet;

pub use limiter::Limiter;
pub use vm::{Program, render};

pub fn build(prelude: &str, src: &str) -> anyhow::Result<Program> {
    let src = format!("{prelude}\n{src}");

    let mut parser = parser::Parser::new(lexer::Lexer::new(&src));
    let items = parser.parse()?;

    let mut env = env::Env::new();
    let resolved_items = env.resolve(&items)?;

    let mut prog = Program::new();
    compiler::compile(&mut prog, &resolved_items)?;

    Ok(prog)
}
