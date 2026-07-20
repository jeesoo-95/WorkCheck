// 릴리스 빌드에서 콘솔 창을 띄우지 않음
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    workcheck_lib::run();
}
