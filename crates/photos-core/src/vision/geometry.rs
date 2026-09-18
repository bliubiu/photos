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
/// 三路融合（髋/膝可用时）头部角权重
pub const TORSO_HEAD_WEIGHT: f64 = 0.5;
/// 三路融合（髋/膝可用时）肩线角权重
pub const TORSO_SHOULDER_WEIGHT: f64 = 0.3;
/// 三路融合（髋/膝可用时）躯干垂直度权重
pub const TORSO_WEIGHT: f64 = 0.2;
/// 侧脸（yaw）告警阈值（度）
pub const SIDE_FACE_YAW_DEG: f64 = 30.0;
/// 俯仰正面基准比：平视正面照下「眼中点→鼻尖」占「眼中点→嘴中点」垂直距离的比例
pub const PITCH_FRONTAL_RATIO: f64 = 0.55;
/// 俯仰告警容差：比例偏离基准超过该值即视为明显低头/仰头
pub const PITCH_RATIO_TOLERANCE: f64 = 0.15;

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

/// 头部角：双眼连线倾角（按图像 x 递增方向规范化）
pub fn head_angle(left_eye: &Point2, right_eye: &Point2) -> f64 {
    horizontal_angle(left_eye, right_eye)
}

/// 肩线角：双肩连线倾角（按图像 x 递增方向规范化）
pub fn shoulder_angle(left_shoulder: &Point2, right_shoulder: &Point2) -> f64 {
    horizontal_angle(left_shoulder, right_shoulder)
}

/// 两点连线相对水平的倾角（逆时针为正，右端抬高为负）。
/// 关键点命名沿用解剖学左右（MoveNet/COCO 约定：`left_*` 指人物自身左侧），
/// 面对面拍摄时与图像左右相反，直接按传入顺序求角会得到 ≈±180° 的伪角，
/// 故此处按图像 x 递增方向规范化，结果落在 ±90°。
fn horizontal_angle(a: &Point2, b: &Point2) -> f64 {
    if a.x <= b.x {
        line_angle(a, b)
    } else {
        line_angle(b, a)
    }
}

/// 融合角：0.6 × 头部角 + 0.4 × 肩线角
pub fn fused_angle(head: f64, shoulder: f64) -> f64 {
    HEAD_WEIGHT * head + SHOULDER_WEIGHT * shoulder
}

/// 躯干垂直度角：肩中点 → 髋（或膝）中点连线相对竖直方向的倾角（度，顺时针为正）。
/// 符号与 `head_angle`/`shoulder_angle` 一致，可与滚动角同向加权融合；
/// 修复高低肩之外的侧身倾斜（下半身相对上半身歪斜）。
pub fn torso_angle(shoulder_mid: &Point2, lower_mid: &Point2) -> f64 {
    let vx = shoulder_mid.x - lower_mid.x;
    let vy = lower_mid.y - shoulder_mid.y; // 屏幕向上分量（下方点更靠下时为正）
    vx.atan2(vy).to_degrees().clamp(-90.0, 90.0)
}

/// 三路融合角：0.5 × 头部角 + 0.3 × 肩线角 + 0.2 × 躯干垂直度
pub fn fused_angle_with_torso(head: f64, shoulder: f64, torso: f64) -> f64 {
    TORSO_HEAD_WEIGHT * head + TORSO_SHOULDER_WEIGHT * shoulder + TORSO_WEIGHT * torso
}

/// 由人脸 5 点关键点（左眼、右眼、鼻尖）估计头部偏转（yaw，度，正负表示左右转）。
/// 正面时鼻尖投影落在双眼中点；转头时鼻尖向偏转方向偏移，偏移量与双眼半间距之比近似
/// `sin(yaw)`（自归一化，不依赖脸框宽度这类随偏转同时收缩的参考量）。
/// 双眼间距退化（< 2px，如桩数据双眼重合）时返回 None（无法判断）。
pub fn yaw_from_landmarks(left_eye: &Point2, right_eye: &Point2, nose: &Point2) -> Option<f64> {
    let half_span = (right_eye.x - left_eye.x).abs() / 2.0;
    if half_span < 1.0 {
        return None;
    }
    let offset = nose.x - (left_eye.x + right_eye.x) / 2.0;
    Some((offset / half_span).clamp(-1.0, 1.0).asin().to_degrees())
}

/// 由人脸 5 点关键点（双眼、鼻尖、双嘴角）估计俯仰比：鼻尖相对眼线的垂直位置占
/// 「眼线→嘴线」垂直距离的比例。正面平视约为 [`PITCH_FRONTAL_RATIO`]；低头时鼻尖下移、
/// 比例增大，仰头时比例减小（比值无量纲，与脸大小无关）。
/// 眼线→嘴线垂直距离退化（< 2px，如演示/桩数据关键点重合）时返回 None（无法判断）。
pub fn pitch_ratio_from_landmarks(
    left_eye: &Point2,
    right_eye: &Point2,
    nose: &Point2,
    left_mouth: &Point2,
    right_mouth: &Point2,
) -> Option<f64> {
    let eye_y = (left_eye.y + right_eye.y) / 2.0;
    let mouth_y = (left_mouth.y + right_mouth.y) / 2.0;
    let span = mouth_y - eye_y;
    if span < 2.0 {
        return None;
    }
    Some((nose.y - eye_y) / span)
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
    fn 关键点左右与图像相反时角度为真实倾角() {
        // MoveNet/COCO 约定：left_* 为人物自身左侧，面对面拍摄时位于图像右侧
        let left_eye = Point2::new(370.0, 140.4);
        let right_eye = Point2::new(330.0, 139.3);
        let a = head_angle(&left_eye, &right_eye);
        assert!(a.abs() < 10.0, "应为小倾角而非 ≈180° 伪角，实际 {a}");
        assert!((a - 1.58).abs() < 0.1, "实际 {a}");
        // 参数顺序颠倒不影响结果（方向已按图像 x 规范化）
        assert!((head_angle(&right_eye, &left_eye) - a).abs() < 1e-9);
        // 肩线同理：图像左肩 x 更小 → 不交换
        let sl = Point2::new(225.0, 312.5);
        let sr = Point2::new(461.4, 312.8);
        let s = shoulder_angle(&sr, &sl);
        assert!((s - 0.07).abs() < 0.1, "实际 {s}");
        assert!((shoulder_angle(&sl, &sr) - s).abs() < 1e-9);
    }

    #[test]
    fn 融合角加权() {
        assert!((fused_angle(10.0, 20.0) - 14.0).abs() < 1e-9);
        assert!((fused_angle(0.0, 0.0)).abs() < 1e-9);
    }

    #[test]
    fn 躯干垂直度角符号与幅值() {
        // 竖直躯干：肩中点与髋中点同 x → 0°
        let s = Point2::new(100.0, 100.0);
        assert!(torso_angle(&s, &Point2::new(100.0, 300.0)).abs() < 1e-9);
        // 肩在髋右侧（身体顺时针倾斜）→ 正值
        let a = torso_angle(&Point2::new(110.0, 100.0), &Point2::new(100.0, 300.0));
        assert!((a - 2.8624).abs() < 1e-3, "实际 {a}");
        // 肩在髋左侧 → 负值
        let b = torso_angle(&Point2::new(90.0, 100.0), &Point2::new(100.0, 300.0));
        assert!((b + 2.8624).abs() < 1e-3, "实际 {b}");
    }

    #[test]
    fn 三路融合角加权() {
        // 0.5×10 + 0.3×20 + 0.2×30 = 17
        assert!((fused_angle_with_torso(10.0, 20.0, 30.0) - 17.0).abs() < 1e-9);
        assert!(
            (TORSO_HEAD_WEIGHT + TORSO_SHOULDER_WEIGHT + TORSO_WEIGHT - 1.0).abs() < 1e-9,
            "三路权重应归一"
        );
    }

    #[test]
    fn 侧脸偏转估计() {
        let l = Point2::new(50.0, 100.0);
        let r = Point2::new(150.0, 100.0);
        // 正面：鼻尖在双眼中点正下方 → 0°
        let frontal = yaw_from_landmarks(&l, &r, &Point2::new(100.0, 140.0)).unwrap();
        assert!(frontal.abs() < 1e-9, "实际 {frontal}");
        // 右转：鼻尖右移 30px（半间距 50px）→ asin(0.6) ≈ 36.87°
        let right = yaw_from_landmarks(&l, &r, &Point2::new(130.0, 140.0)).unwrap();
        assert!((right - 36.8699).abs() < 1e-3, "实际 {right}");
        // 左转符号相反
        let left = yaw_from_landmarks(&l, &r, &Point2::new(70.0, 140.0)).unwrap();
        assert!((left + 36.8699).abs() < 1e-3, "实际 {left}");
        assert!(right.abs() > SIDE_FACE_YAW_DEG && left.abs() > SIDE_FACE_YAW_DEG);
        // 双眼重合（桩数据退化）→ 无法判断
        assert!(yaw_from_landmarks(&l, &l, &Point2::new(100.0, 140.0)).is_none());
    }

    #[test]
    fn 俯仰比估计() {
        let le = Point2::new(50.0, 100.0);
        let re = Point2::new(150.0, 100.0);
        let lm = Point2::new(70.0, 200.0);
        let rm = Point2::new(130.0, 200.0);
        // 眼线 y=100、嘴线 y=200：鼻尖 y=155 → 比例 0.55（正面基准）
        let frontal =
            pitch_ratio_from_landmarks(&le, &re, &Point2::new(100.0, 155.0), &lm, &rm).unwrap();
        assert!(
            (frontal - PITCH_FRONTAL_RATIO).abs() < 1e-9,
            "实际 {frontal}"
        );
        assert!((frontal - PITCH_FRONTAL_RATIO).abs() <= PITCH_RATIO_TOLERANCE);
        // 低头：鼻尖下移到 y=185 → 比例 0.85，超出容差
        let down =
            pitch_ratio_from_landmarks(&le, &re, &Point2::new(100.0, 185.0), &lm, &rm).unwrap();
        assert!((down - 0.85).abs() < 1e-9, "实际 {down}");
        assert!(down - PITCH_FRONTAL_RATIO > PITCH_RATIO_TOLERANCE);
        // 仰头：鼻尖上移到 y=125 → 比例 0.25，超出容差且方向相反
        let up =
            pitch_ratio_from_landmarks(&le, &re, &Point2::new(100.0, 125.0), &lm, &rm).unwrap();
        assert!((up - 0.25).abs() < 1e-9, "实际 {up}");
        assert!(PITCH_FRONTAL_RATIO - up > PITCH_RATIO_TOLERANCE);
        // 眼线→嘴线垂直距离退化（关键点重合）→ 无法判断
        assert!(pitch_ratio_from_landmarks(&le, &re, &le, &le, &re).is_none());
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
