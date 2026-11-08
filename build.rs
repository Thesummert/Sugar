fn main() {
    let mut config = cpp_build::Config::new();
    config.include(std::env::var("DEP_QT_INCLUDE_PATH").expect("需要安装 Qt 开发环境"));
    for flag in std::env::var("DEP_QT_COMPILE_FLAGS")
        .unwrap()
        .split_terminator(';')
    {
        config.flag(flag);
    }
    config.flag_if_supported("-std=c++17");
    config.flag_if_supported("/std:c++17");
    config.define("QT_NO_OPENGL", None);
    config.build("src/native_drag.rs");
    println!("cargo:rerun-if-changed=src/native_drag.rs");
    slint_build::compile("ui/app.slint").expect("failed to compile UI");
}
