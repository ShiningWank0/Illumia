// デスクトップ dev 用のエントリ (Android では lib.rs の mobile_entry_point を使う)。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    illumia_android_lib::run();
}
