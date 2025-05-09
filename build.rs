// build.rs
fn main() {
    slint_build::compile("ui/common.slint").unwrap();
    slint_build::compile("ui/styles.slint").unwrap(); // Compile styles
    slint_build::compile("ui/coincard.slint").unwrap();
    slint_build::compile("ui/appwindow.slint").unwrap();
}