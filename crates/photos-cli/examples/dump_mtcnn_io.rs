//! 临时诊断：打印 MTCNN 三模型输入/输出形状。运行：
//! cargo run -q -p photos-cli --example dump_mtcnn_io
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for name in ["pnet", "rnet", "onet"] {
        let path = format!("models/{name}.onnx");
        let mut s = ort::session::Session::builder()?.commit_from_file(&path)?;
        println!("== {name} ==");
        for i in s.inputs() {
            println!("  in  {} {:?}", i.name, i.shape());
        }
        for o in s.outputs() {
            println!("  out {} {:?}", o.name, o.shape());
        }
        // 试跑错误尺寸，打印 ORT 报错
        let _ = &mut s;
    }
    Ok(())
}
