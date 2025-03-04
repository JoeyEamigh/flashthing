run:
  cargo run -p flashthing-cli

build:
  cargo build --release
  bun run build

bindings:
  bun run build

binding-example:
  bun run dev
  cd bindings && bun run example

tokei:
  tokei -t Rust,TypeScript,TSX