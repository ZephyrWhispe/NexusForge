// NexusForge 入口：仅调用 lib 的 run()，便于集成测试复用
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    nexusforge_lib::run()
}
