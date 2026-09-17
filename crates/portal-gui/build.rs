fn main() {
    assert!(std::path::Path::new("frontend/dist/index.html").exists(), "先执行 pnpm --dir crates/portal-gui/frontend install && pnpm --dir crates/portal-gui/frontend build");
    tauri_build::build();
}
