//! Bash 超时规则的单一权威：前台默认/上限、后台时限、无人值守默认值与环境变量覆盖
//! 全部从 [`ShellTimeoutPolicy`] 推导，调用方不得各自维护数值。
//!
//! 规则对齐 Claude Code 工具参考（Bash tool behavior / Time limit for background
//! commands）：
//! - 前台：默认 [`FOREGROUND_DEFAULT_TIMEOUT_MS`]，请求上限 [`FOREGROUND_MAX_TIMEOUT_MS`]，
//!   生效上限取二者（含环境变量覆盖）的较大值；
//! - 有人值守会话：后台任务不设时限，`timeout` 对后台不生效；
//! - 无人值守会话：后台任务有时限（默认 30 分钟，print 文本 prompt 为 10 分钟，
//!   显式 `timeout` 最大 2 小时），前台超时转入后台的任务从转入时起算默认时限；
//! - 环境变量 `BASH_DEFAULT_TIMEOUT_MS` / `BASH_MAX_TIMEOUT_MS` 只能调高后台默认与上限。

use peri_acp_types::session_attendance::SessionAttendance;

/// 前台未传 `timeout` 时的默认超时（`BASH_DEFAULT_TIMEOUT_MS` 可覆盖）。
pub const FOREGROUND_DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// 模型可请求的前台超时上限（`BASH_MAX_TIMEOUT_MS` 可覆盖）；生效上限取它与
/// 前台默认的较大值，超过的请求按生效上限执行。
pub const FOREGROUND_MAX_TIMEOUT_MS: u64 = 600_000;

/// 无人值守会话后台任务的默认时限。
pub const BACKGROUND_DEFAULT_TIMEOUT_MS: u64 = 1_800_000;

/// 无人值守且 prompt 以文本传入（print）的后台默认时限。
pub const BACKGROUND_TEXT_PROMPT_DEFAULT_TIMEOUT_MS: u64 = 600_000;

/// 无人值守会话后台显式 `timeout` 的上限（生效前台上限更大时跟随它）。
pub const BACKGROUND_MAX_TIMEOUT_MS: u64 = 7_200_000;

/// 前台默认超时的环境变量名。
pub const DEFAULT_TIMEOUT_ENV: &str = "BASH_DEFAULT_TIMEOUT_MS";

/// 前台请求上限的环境变量名。
pub const MAX_TIMEOUT_ENV: &str = "BASH_MAX_TIMEOUT_MS";

/// 显式 timeout 的下限：Windows 进程创建/终止开销大，过短超时不可靠。
fn min_timeout_ms() -> u64 {
    if cfg!(target_os = "windows") {
        5_000
    } else {
        1
    }
}

/// timeout 参数不满足数值契约。
///
/// 缺失/`null` 表示未设置，不属于错误；字符串、bool、对象、数组、负数与小数
/// 一律报错，不再静默回落到默认值。整数值浮点（如 `15000.0`）归一化为整数。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid `{parameter}`: expected an integer number of milliseconds >= 0 (null or omitted means unset); received {received}"
)]
pub struct TimeoutParseError {
    parameter: &'static str,
    received: String,
}

/// 严格解析 `timeout` 字段（毫秒）。
///
/// - 缺失 / `null` → `Ok(None)`（调用方决定默认语义）
/// - 非负整数 → `Ok(Some(ms))`
/// - 整数值浮点（`15000.0`）→ 归一化为 `Ok(Some(15000))`，兼容旧 `number` schema
/// - 小数、负数、字符串、bool、对象、数组 → [`TimeoutParseError`]
pub fn parse_timeout_ms(
    input: &serde_json::Value,
    parameter: &'static str,
) -> Result<Option<u64>, TimeoutParseError> {
    let Some(value) = input.get(parameter) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let parsed = value.as_u64().or_else(|| {
        let number = value.as_f64()?;
        (number.is_finite() && number.fract() == 0.0 && number >= 0.0 && number <= u64::MAX as f64)
            .then_some(number as u64)
    });
    parsed.map(Some).ok_or_else(|| TimeoutParseError {
        parameter,
        received: value.to_string(),
    })
}

/// 解析环境变量毫秒值：正整数，兼容 `64_000` 与 `2e3` 写法；其余（空、0、负、
/// 小数、非数字）视为非法，调用方忽略并使用默认。
fn parse_env_ms(raw: &str) -> Option<u64> {
    let cleaned: String = raw.trim().chars().filter(|c| *c != '_').collect();
    let value = cleaned.parse::<u64>().ok().or_else(|| {
        let number = cleaned.parse::<f64>().ok()?;
        (number.is_finite() && number.fract() == 0.0 && number >= 1.0 && number <= u64::MAX as f64)
            .then_some(number as u64)
    })?;
    (value > 0).then_some(value)
}

/// 命令是否以 `sleep` 开头（首个词）。这类命令前台超时直接终止，不转入后台：
/// 转后台只会让一个纯等待命令继续空转。
pub fn is_sleep_command(command: &str) -> bool {
    command.split_whitespace().next() == Some("sleep")
}

/// 后台 timeout 的解析结果。
///
/// `requested` 仅在模型请求值**没有按原值生效**时为 `Some`，供回执如实说明：
/// - `limit_ms == None`：有人值守会话，后台不限时，请求值被忽略；
/// - `limit_ms == Some(_)` 且 `requested == Some(0)`：`0` 不表示不限时，改用默认时限；
/// - `limit_ms == Some(_)` 且 `requested > 0`：超过上限，按上限执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackgroundTimeout {
    pub limit_ms: Option<u64>,
    pub requested: Option<u64>,
}

/// 一次会话的 shell 超时规则：值守形态 + 环境变量覆盖，构造后不可变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellTimeoutPolicy {
    attendance: SessionAttendance,
    env_default_ms: Option<u64>,
    env_max_ms: Option<u64>,
    /// 测试夹具缩短转后台时限，避免等满 30 分钟；生产构建不存在该字段。
    #[cfg(any(test, feature = "test-support"))]
    moved_limit_override_ms: Option<u64>,
}

impl ShellTimeoutPolicy {
    /// 读取进程环境变量构造规则；环境变量的唯一读取点。
    pub fn from_env(attendance: SessionAttendance) -> Self {
        Self::from_lookup(attendance, |name| std::env::var(name).ok())
    }

    /// 以显式查找函数构造规则（测试与非进程环境来源）；非法值按未设置处理。
    pub fn from_lookup(
        attendance: SessionAttendance,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Self {
        Self {
            attendance,
            env_default_ms: lookup(DEFAULT_TIMEOUT_ENV).and_then(|raw| parse_env_ms(&raw)),
            env_max_ms: lookup(MAX_TIMEOUT_ENV).and_then(|raw| parse_env_ms(&raw)),
            #[cfg(any(test, feature = "test-support"))]
            moved_limit_override_ms: None,
        }
    }

    /// 测试专用：把转后台时限固定为 `ms`（规则本身只允许环境变量调高）。
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_moved_limit_for_test(mut self, ms: u64) -> Self {
        self.moved_limit_override_ms = Some(ms);
        self
    }

    pub fn attendance(&self) -> SessionAttendance {
        self.attendance
    }

    /// 前台未传 `timeout` 时的默认超时。
    pub fn foreground_default_ms(&self) -> u64 {
        self.env_default_ms.unwrap_or(FOREGROUND_DEFAULT_TIMEOUT_MS)
    }

    /// 前台生效上限：请求上限与前台默认的较大值。
    pub fn foreground_ceiling_ms(&self) -> u64 {
        self.foreground_default_ms()
            .max(self.env_max_ms.unwrap_or(FOREGROUND_MAX_TIMEOUT_MS))
    }

    /// 后台默认时限；有人值守会话不限时（`None`）。环境变量只能调高。
    pub fn background_default_ms(&self) -> Option<u64> {
        let builtin = match self.attendance {
            SessionAttendance::Attended => return None,
            SessionAttendance::Unattended => BACKGROUND_DEFAULT_TIMEOUT_MS,
            SessionAttendance::UnattendedTextPrompt => BACKGROUND_TEXT_PROMPT_DEFAULT_TIMEOUT_MS,
        };
        Some(builtin.max(self.env_default_ms.unwrap_or(0)))
    }

    /// 后台显式 `timeout` 的上限；有人值守会话不限时（`None`）。
    /// 前台生效上限超过 2 小时时，后台上限随之提高。
    pub fn background_max_ms(&self) -> Option<u64> {
        self.attendance
            .is_unattended()
            .then(|| BACKGROUND_MAX_TIMEOUT_MS.max(self.foreground_ceiling_ms()))
    }

    /// 前台超时转入后台的任务时限（从转入时起算）；有人值守会话不限时。
    pub fn moved_to_background_limit_ms(&self) -> Option<u64> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(ms) = self.moved_limit_override_ms {
            return Some(ms);
        }
        self.background_default_ms()
    }

    /// 解析前台（同步）timeout：结果恒为有界值。
    ///
    /// 返回 `(生效毫秒, 请求值被改写时的原始值)`：
    /// - 未传 → 前台默认，未改写
    /// - 显式 `0` → 前台生效上限（`0` 不能表示"不超时"），原始值为 `Some(0)`
    /// - 显式 > 前台生效上限 → 上限值，原始值为请求值（供回执说明）
    /// - 其余 → 请求值，并按平台下限兜底（Unix 1ms / Windows 5000ms）
    pub fn parse_foreground(
        &self,
        input: &serde_json::Value,
    ) -> Result<(u64, Option<u64>), TimeoutParseError> {
        let ceiling = self.foreground_ceiling_ms();
        match parse_timeout_ms(input, "timeout")? {
            None => Ok((self.foreground_default_ms(), None)),
            Some(0) => Ok((ceiling, Some(0))),
            Some(ms) if ms > ceiling => Ok((ceiling, Some(ms))),
            Some(ms) => Ok((ms.max(min_timeout_ms()), None)),
        }
    }

    /// 解析后台 timeout，规则见 [`BackgroundTimeout`]。
    pub fn parse_background(
        &self,
        input: &serde_json::Value,
    ) -> Result<BackgroundTimeout, TimeoutParseError> {
        let requested = parse_timeout_ms(input, "timeout")?;
        let (Some(default), Some(max)) = (self.background_default_ms(), self.background_max_ms())
        else {
            // `0` 在这里与"不限时"同义，不需要向模型解释。
            return Ok(BackgroundTimeout {
                limit_ms: None,
                requested: requested.filter(|ms| *ms > 0),
            });
        };
        Ok(match requested {
            None => BackgroundTimeout {
                limit_ms: Some(default),
                requested: None,
            },
            Some(0) => BackgroundTimeout {
                limit_ms: Some(default),
                requested: Some(0),
            },
            Some(ms) if ms > max => BackgroundTimeout {
                limit_ms: Some(max),
                requested: Some(ms),
            },
            Some(ms) => BackgroundTimeout {
                limit_ms: Some(ms.max(min_timeout_ms())),
                requested: None,
            },
        })
    }
}
