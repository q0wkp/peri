//! Bash 超时规则（`ShellTimeoutPolicy`）：前台、后台、无人值守与环境变量覆盖。
//!
//! 全部经 `from_lookup` 注入环境，不读写真实进程环境变量。

use super::*;
use peri_acp_types::session_attendance::SessionAttendance as Attendance;
use serde_json::json;

fn policy(attendance: Attendance, env: &[(&str, &str)]) -> ShellTimeoutPolicy {
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    ShellTimeoutPolicy::from_lookup(attendance, move |name| {
        env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    })
}

fn min_ms() -> u64 {
    if cfg!(target_os = "windows") {
        5_000
    } else {
        1
    }
}

const DEFAULT_ENV: &str = DEFAULT_TIMEOUT_ENV;
const MAX_ENV: &str = MAX_TIMEOUT_ENV;

// ── 前台 ────────────────────────────────────────────────────────────────

/// 前台：未传用默认 2 分钟；显式值生效；`0` 与超上限都界到上限并回传请求值。
#[test]
fn foreground_default_cap_and_zero() {
    let p = policy(Attendance::Attended, &[]);
    assert_eq!(p.foreground_default_ms(), 120_000);
    assert_eq!(p.foreground_ceiling_ms(), 600_000);
    assert_eq!(p.parse_foreground(&json!({})).unwrap(), (120_000, None));
    assert_eq!(
        p.parse_foreground(&json!({"timeout": null})).unwrap(),
        (120_000, None)
    );
    assert_eq!(
        p.parse_foreground(&json!({"timeout": 2000})).unwrap(),
        (2000.max(min_ms()), None)
    );
    assert_eq!(
        p.parse_foreground(&json!({"timeout": 600_000})).unwrap(),
        (600_000, None),
        "恰好等于上限不算改写"
    );
    assert_eq!(
        p.parse_foreground(&json!({"timeout": 3_000_000})).unwrap(),
        (600_000, Some(3_000_000)),
        "超上限按上限执行，并回传请求值"
    );
    assert_eq!(
        p.parse_foreground(&json!({"timeout": 0})).unwrap(),
        (600_000, Some(0)),
        "`0` 不能表示不超时"
    );
    assert_eq!(
        p.parse_foreground(&json!({"timeout": 15000.0})).unwrap(),
        (15_000, None),
        "整数值浮点归一化为整数毫秒"
    );
}

/// 前台生效上限 = max(BASH_DEFAULT_TIMEOUT_MS, BASH_MAX_TIMEOUT_MS)。
#[test]
fn foreground_env_overrides_default_and_ceiling() {
    let p = policy(Attendance::Attended, &[(DEFAULT_ENV, "300000")]);
    assert_eq!(p.parse_foreground(&json!({})).unwrap(), (300_000, None));
    assert_eq!(p.foreground_ceiling_ms(), 600_000);

    let p = policy(Attendance::Attended, &[(MAX_ENV, "900000")]);
    assert_eq!(p.foreground_ceiling_ms(), 900_000);
    assert_eq!(
        p.parse_foreground(&json!({"timeout": 800_000})).unwrap(),
        (800_000, None)
    );
    assert_eq!(
        p.parse_foreground(&json!({"timeout": 1_000_000})).unwrap(),
        (900_000, Some(1_000_000))
    );

    // 默认值高于请求上限时，上限跟随默认值（取较大者）。
    let p = policy(Attendance::Attended, &[(DEFAULT_ENV, "1200000")]);
    assert_eq!(p.foreground_ceiling_ms(), 1_200_000);

    // 较小的 BASH_MAX 不能压低内置上限以下的默认值；上限仍不低于默认。
    let p = policy(Attendance::Attended, &[(MAX_ENV, "60000")]);
    assert_eq!(p.foreground_default_ms(), 120_000);
    assert_eq!(p.foreground_ceiling_ms(), 120_000);
}

/// 非法环境变量值被忽略并使用默认，`64_000` / `2e5` 写法被接受。
#[test]
fn env_invalid_values_are_ignored() {
    for bad in ["", "abc", "0", "-5", "1.5", "NaN", "inf", "0x10"] {
        let p = policy(Attendance::Attended, &[(DEFAULT_ENV, bad), (MAX_ENV, bad)]);
        assert_eq!(p.foreground_default_ms(), 120_000, "{bad:?}");
        assert_eq!(p.foreground_ceiling_ms(), 600_000, "{bad:?}");
    }
    let p = policy(Attendance::Attended, &[(DEFAULT_ENV, "64_000")]);
    assert_eq!(p.foreground_default_ms(), 64_000);
    let p = policy(Attendance::Attended, &[(DEFAULT_ENV, " 2e5 ")]);
    assert_eq!(p.foreground_default_ms(), 200_000);
}

#[test]
fn timeout_parameter_must_be_a_non_negative_integer() {
    let p = policy(Attendance::Attended, &[]);
    for invalid in [
        json!({"timeout": "1000"}),
        json!({"timeout": -5}),
        json!({"timeout": 1.5}),
        json!({"timeout": true}),
        json!({"timeout": {}}),
        json!({"timeout": []}),
    ] {
        for message in [
            p.parse_foreground(&invalid).unwrap_err().to_string(),
            p.parse_background(&invalid).unwrap_err().to_string(),
        ] {
            assert!(
                message.contains("timeout") && message.contains("received"),
                "错误应包含参数名与收到值: {message}"
            );
        }
    }
}

// ── 后台：有人值守 ───────────────────────────────────────────────────────

/// 有人值守：后台没有时限，`timeout` 不生效且不再被压成 10 分钟。
#[test]
fn attended_background_has_no_limit_and_ignores_timeout() {
    let p = policy(Attendance::Attended, &[(DEFAULT_ENV, "5000000")]);
    assert_eq!(p.background_default_ms(), None);
    assert_eq!(p.background_max_ms(), None);
    assert_eq!(p.moved_to_background_limit_ms(), None);
    assert_eq!(
        p.parse_background(&json!({})).unwrap(),
        BackgroundTimeout {
            limit_ms: None,
            requested: None
        }
    );
    assert_eq!(
        p.parse_background(&json!({"timeout": 3_000_000})).unwrap(),
        BackgroundTimeout {
            limit_ms: None,
            requested: Some(3_000_000)
        },
        "请求值被忽略，且作为未生效请求回传以便回执说明"
    );
    assert_eq!(
        p.parse_background(&json!({"timeout": 0})).unwrap(),
        BackgroundTimeout {
            limit_ms: None,
            requested: None
        },
        "`0` 与不限时同义，无需说明"
    );
}

// ── 后台：无人值守 ───────────────────────────────────────────────────────

/// 无人值守：默认 30 分钟；显式 timeout 生效（含 3000000），最大 2 小时。
#[test]
fn unattended_background_default_explicit_and_max() {
    let p = policy(Attendance::Unattended, &[]);
    assert_eq!(p.background_default_ms(), Some(1_800_000));
    assert_eq!(p.background_max_ms(), Some(7_200_000));
    assert_eq!(
        p.parse_background(&json!({})).unwrap(),
        BackgroundTimeout {
            limit_ms: Some(1_800_000),
            requested: None
        }
    );
    assert_eq!(
        p.parse_background(&json!({"timeout": 2000})).unwrap(),
        BackgroundTimeout {
            limit_ms: Some(2000.max(min_ms())),
            requested: None
        }
    );
    assert_eq!(
        p.parse_background(&json!({"timeout": 3_000_000})).unwrap(),
        BackgroundTimeout {
            limit_ms: Some(3_000_000),
            requested: None
        },
        "3000000 必须按原值生效，不能被静默压成 10 分钟"
    );
    assert_eq!(
        p.parse_background(&json!({"timeout": 7_200_000})).unwrap(),
        BackgroundTimeout {
            limit_ms: Some(7_200_000),
            requested: None
        }
    );
    assert_eq!(
        p.parse_background(&json!({"timeout": 9_000_000})).unwrap(),
        BackgroundTimeout {
            limit_ms: Some(7_200_000),
            requested: Some(9_000_000)
        },
        "超上限按上限执行，并回传请求值"
    );
    assert_eq!(
        p.parse_background(&json!({"timeout": 0})).unwrap(),
        BackgroundTimeout {
            limit_ms: Some(1_800_000),
            requested: Some(0)
        },
        "无人值守下 `0` 不能表示不限时"
    );
    assert_eq!(p.moved_to_background_limit_ms(), Some(1_800_000));
}

/// print 文本 prompt：直接后台与前台转后台的默认都是 10 分钟；上限仍是 2 小时。
#[test]
fn unattended_text_prompt_defaults_to_ten_minutes() {
    let p = policy(Attendance::UnattendedTextPrompt, &[]);
    assert_eq!(p.background_default_ms(), Some(600_000));
    assert_eq!(p.moved_to_background_limit_ms(), Some(600_000));
    assert_eq!(p.background_max_ms(), Some(7_200_000));
    assert_eq!(
        p.parse_background(&json!({})).unwrap().limit_ms,
        Some(600_000)
    );
    assert_eq!(
        p.parse_background(&json!({"timeout": 3_000_000}))
            .unwrap()
            .limit_ms,
        Some(3_000_000)
    );
}

/// 环境变量只能调高后台默认与上限，不能调低。
#[test]
fn env_only_raises_background_limits() {
    // 30 分钟默认：低于或等于 1800000 不生效，高于则替换。
    for (value, expected) in [
        ("1000000", 1_800_000),
        ("1800000", 1_800_000),
        ("3600000", 3_600_000),
    ] {
        let p = policy(Attendance::Unattended, &[(DEFAULT_ENV, value)]);
        assert_eq!(p.background_default_ms(), Some(expected), "{value}");
        assert_eq!(p.moved_to_background_limit_ms(), Some(expected), "{value}");
        assert_eq!(
            p.parse_background(&json!({})).unwrap().limit_ms,
            Some(expected)
        );
    }
    // 文本 prompt：高于 600000 即替换 10 分钟默认。
    for (value, expected) in [
        ("500000", 600_000),
        ("600000", 600_000),
        ("700000", 700_000),
    ] {
        let p = policy(Attendance::UnattendedTextPrompt, &[(DEFAULT_ENV, value)]);
        assert_eq!(p.background_default_ms(), Some(expected), "{value}");
    }
    // 2 小时上限：BASH_MAX_TIMEOUT_MS 或 BASH_DEFAULT_TIMEOUT_MS 超过它时提升；更小的值不降低。
    let p = policy(Attendance::Unattended, &[(MAX_ENV, "10000000")]);
    assert_eq!(p.background_max_ms(), Some(10_000_000));
    assert_eq!(
        p.parse_background(&json!({"timeout": 9_000_000}))
            .unwrap()
            .limit_ms,
        Some(9_000_000)
    );
    let p = policy(Attendance::Unattended, &[(DEFAULT_ENV, "9000000")]);
    assert_eq!(p.background_max_ms(), Some(9_000_000));
    assert_eq!(p.background_default_ms(), Some(9_000_000));
    let p = policy(Attendance::Unattended, &[(MAX_ENV, "1000000")]);
    assert_eq!(p.background_max_ms(), Some(7_200_000));
    // 非法值被忽略，保持内置默认。
    let p = policy(
        Attendance::Unattended,
        &[(DEFAULT_ENV, "oops"), (MAX_ENV, "-1")],
    );
    assert_eq!(p.background_default_ms(), Some(1_800_000));
    assert_eq!(p.background_max_ms(), Some(7_200_000));
}

// ── sleep 前缀 ───────────────────────────────────────────────────────────

#[test]
fn sleep_prefixed_commands_are_not_moved_to_background() {
    for command in ["sleep 5", "  sleep 1 && echo done", "sleep", "sleep\t3"] {
        assert!(is_sleep_command(command), "{command:?}");
    }
    for command in [
        "sleepwatcher --run",
        "echo sleep 5",
        "sh -c 'sleep 1'",
        "",
        "cargo build",
    ] {
        assert!(!is_sleep_command(command), "{command:?}");
    }
}
