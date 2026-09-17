//! 姿态角度求解与纠偏决策（复刻 LiYing：头部角 0.6 + 肩线角 0.4 加权融合）。
//!
//! 自动阈值保护：|θ_final| > 22° 不自动纠偏、降级继续出图并返回告警；
//! 手动覆盖：用户显式角度，上限 ±45°。

use crate::error::{CoreError, CoreResult};

/// 自动纠偏角度上限（度）
pub const AUTO_THRESHOLD_DEG: f64 = 22.0;
/// 手动纠偏角度上限（度）
pub const MANUAL_LIMIT_DEG: f64 = 45.0;
/// 头部角权重
pub const HEAD_WEIGHT: f64 = 0.6;
/// 肩线角权重
pub const SHOULDER_WEIGHT: f64 = 0.4;

/// 平面点（图像坐标，x 向右、y 向下）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

impl Point2 {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// 两点连线与水平线的夹角（度，逆时针为正）
pub fn line_angle(p1: &Point2, p2: &Point2) -> f64 {
    (p2.y - p1.y).atan2(p2.x - p1.x).to_degrees()
}

/// 头部角：双眼连线（左眼 → 右眼）
pub fn head_angle(left_eye: &Point2, right_eye: &Point2) -> f64 {
    line_angle(left_eye, right_eye)
}

/// 肩线角：双肩连线（左肩 → 右肩）
pub fn shoulder_angle(left_shoulder: &Point2, right_shoulder: &Point2) -> f64 {
    line_angle(left_shoulder, right_shoulder)
}

/// 融合角：0.6 × 头部角 + 0.4 × 肩线角
pub fn fused_angle(head: f64, shoulder: f64) -> f64 {
    HEAD_WEIGHT * head + SHOULDER_WEIGHT * shoulder
}

/// 纠偏决策结果
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RotationDecision {
    /// 自动纠偏（携带测量角，度）
    Auto(f64),
    /// 超限不纠偏、降级继续出图（携带测量角，度）
    Degraded { measured: f64 },
    /// 手动覆盖（携带用户角度，度）
    Manual(f64),
}

impl RotationDecision {
    /// 需施加的校正角（度）：图像按该角度旋转后扶正（顺时针为正，即测量角取负）
    pub fn correction(&self) -> f64 {
        match self {
            RotationDecision::Auto(a) | RotationDecision::Manual(a) => -a,
            RotationDecision::Degraded { .. } => 0.0,
        }
    }

    /// 告警（降级出图时返回）
    pub fn warning(&self) -> Option<String> {
        match self {
            RotationDecision::Degraded { measured } => {
                Some(format!("角度超限，本次未自动纠偏（测量角 {measured:.1}°）"))
            }
            _ => None,
        }
    }
}

/// 依据测量角与可选手动角度作出纠偏决策
pub fn decide_rotation(measured: f64, manual: Option<f64>) -> CoreResult<RotationDecision> {
    if let Some(m) = manual {
        if m.abs() > MANUAL_LIMIT_DEG {
            return Err(CoreError::ConfigValidate(format!(
                "手动纠偏角度 {m:.1}° 超过上限 ±{MANUAL_LIMIT_DEG}°"
            )));
        }
        return Ok(RotationDecision::Manual(m));
    }
    if measured.abs() > AUTO_THRESHOLD_DEG {
        Ok(RotationDecision::Degraded { measured })
    } else {
        Ok(RotationDecision::Auto(measured))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 水平线角度为零() {
        let l = Point2::new(0.0, 10.0);
        let r = Point2::new(100.0, 10.0);
        assert!((line_angle(&l, &r)).abs() < 1e-6);
    }

    #[test]
    fn 倾斜线角度符号与幅值() {
        // 右端上移 10px（视觉逆时针）→ 角度为负
        let l = Point2::new(0.0, 20.0);
        let r = Point2::new(100.0, 10.0);
        let a = line_angle(&l, &r);
        assert!((a + 5.7106).abs() < 1e-3, "实际 {a}");
    }

    #[test]
    fn 融合角加权() {
        assert!((fused_angle(10.0, 20.0) - 14.0).abs() < 1e-9);
        assert!((fused_angle(0.0, 0.0)).abs() < 1e-9);
    }

    #[test]
    fn 阈值内自动纠偏() {
        assert_eq!(
            decide_rotation(21.9, None).unwrap(),
            RotationDecision::Auto(21.9)
        );
        // 恰好 22° 仍自动
        assert_eq!(
            decide_rotation(22.0, None).unwrap(),
            RotationDecision::Auto(22.0)
        );
        // 校正角为测量角的相反数（扶正）
        let d = decide_rotation(10.0, None).unwrap();
        assert!((d.correction() + 10.0).abs() < 1e-9);
        assert!(d.warning().is_none());
    }

    #[test]
    fn 超限降级出图并告警() {
        let d = decide_rotation(22.1, None).unwrap();
        assert_eq!(d, RotationDecision::Degraded { measured: 22.1 });
        assert_eq!(d.correction(), 0.0);
        let w = d.warning().unwrap();
        assert!(w.contains("未自动纠偏"));
        assert!(w.contains("22.1"));
    }

    #[test]
    fn 手动覆盖边界() {
        assert_eq!(
            decide_rotation(50.0, Some(45.0)).unwrap(),
            RotationDecision::Manual(45.0)
        );
        assert_eq!(
            decide_rotation(0.0, Some(-45.0)).unwrap(),
            RotationDecision::Manual(-45.0)
        );
        assert!(decide_rotation(0.0, Some(45.1)).is_err());
        assert!(decide_rotation(0.0, Some(-45.1)).is_err());
    }

    #[test]
    fn 手动覆盖时校正角为负用户角() {
        let d = decide_rotation(10.0, Some(30.0)).unwrap();
        assert!((d.correction() + 30.0).abs() < 1e-9);
    }
}
