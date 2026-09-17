//! 视觉算子：姿态角度、同步仿射纠偏、人脸检测后处理、关键点解析、抠图后处理、混色、裁切、美颜。
//! 纯 Rust 实现（image + imageproc + nalgebra + 自研算子），见 docs/adr/0001-纯Rust图像栈.md。

pub mod affine;
pub mod beauty;
pub mod blend;
pub mod crop;
pub mod dressing;
pub mod face;
pub mod geometry;
pub mod keypoint;
pub mod layout;
pub mod matting;
