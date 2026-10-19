//! 时间换算：DOS 日期时间按本地时区转 Unix 秒（zip 规范没有时区，写入方用的是本地时间）；
//! NTFS FILETIME 与 Unix 时间戳扩展字段则是绝对时刻，直接换算。

/// FILETIME 纪元（1601-01-01）到 Unix 纪元的秒数。
const FILETIME_EPOCH_DIFF: i64 = 11_644_473_600;

/// 小时级缓存上限：超过就清空重来，防止畸形包里成千上万个不同日期把缓存撑大。
const HOUR_CACHE_MAX: usize = 4096;

thread_local! {
    /// (DOS 日期, 小时) → 该小时整点的 Unix 秒。
    static HOUR_CACHE: std::cell::RefCell<std::collections::HashMap<u32, Option<i64>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// 把 DOS 日期（`date`）与时间（`time`）字段按本地时区转成 Unix 秒。字段非法返回 None。
/// 为什么按小时缓存：`mktime` 一次约 1 µs，10 万条目就要 100 ms，吃掉整个索引预算；
/// 夏令时切换总发生在整点，所以同一（日期, 小时）内的偏移恒定，分秒直接做加法即可。
pub fn dos_to_unix(time: u16, date: u16) -> Option<i64> {
    let year = 1980 + (date >> 9) as i32;
    let month = ((date >> 5) & 0x0F) as u32;
    let day = (date & 0x1F) as u32;
    let hour = (time >> 11) as u32;
    let minute = ((time >> 5) & 0x3F) as u32;
    let second = ((time & 0x1F) as u32) * 2;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let key = (date as u32) << 5 | hour;
    let base = HOUR_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(v) = cache.get(&key) {
            return *v;
        }
        if cache.len() >= HOUR_CACHE_MAX {
            cache.clear();
        }
        let v = local_to_unix(year, month, day, hour, 0, 0);
        cache.insert(key, v);
        v
    })?;
    Some(base + minute as i64 * 60 + second as i64)
}

/// NTFS FILETIME（1601 年起的 100 纳秒数）转 Unix 秒。
pub fn filetime_to_unix(ft: u64) -> Option<i64> {
    let secs = i64::try_from(ft / 10_000_000).ok()?;
    secs.checked_sub(FILETIME_EPOCH_DIFF)
}

/// 本地时间 → Unix 秒。Unix 平台交给 libc 的 `mktime`（它懂时区数据库与夏令时），
/// 其它平台退化为按 UTC 计算。
#[cfg(unix)]
fn local_to_unix(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Option<i64> {
    use std::ffi::{c_char, c_int, c_long};

    /// POSIX `struct tm`；macOS/glibc/musl 在 9 个 int 之后还有 gmtoff 与 zone，
    /// `mktime` 会回写它们，所以结构体必须按完整布局声明，否则会越界写。
    #[repr(C)]
    struct Tm {
        tm_sec: c_int,
        tm_min: c_int,
        tm_hour: c_int,
        tm_mday: c_int,
        tm_mon: c_int,
        tm_year: c_int,
        tm_wday: c_int,
        tm_yday: c_int,
        tm_isdst: c_int,
        tm_gmtoff: c_long,
        tm_zone: *const c_char,
    }

    extern "C" {
        fn mktime(tm: *mut Tm) -> c_long;
    }

    let mut tm = Tm {
        tm_sec: second as c_int,
        tm_min: minute as c_int,
        tm_hour: hour as c_int,
        tm_mday: day as c_int,
        tm_mon: (month - 1) as c_int,
        tm_year: year - 1900,
        tm_wday: 0,
        tm_yday: 0,
        // -1 让 mktime 自己判断夏令时
        tm_isdst: -1,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };
    // 安全性：tm 是栈上完整初始化的结构体，mktime 只在调用期间访问它
    let t = unsafe { mktime(&mut tm) };
    if t == -1 {
        return None;
    }
    Some(t as i64)
}

/// 非 Unix 平台：按 UTC 计算（Howard Hinnant 的 days_from_civil）。
#[cfg(not(unix))]
fn local_to_unix(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Option<i64> {
    Some(utc_to_unix(year, month, day, hour, minute, second))
}

/// 公历日期（UTC）→ Unix 秒，无时区依赖，测试与非 Unix 平台用。
#[allow(dead_code)]
pub fn utc_to_unix(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 非法月份/日期返回 None，不 panic。
    #[test]
    fn rejects_invalid_dos_fields() {
        assert_eq!(dos_to_unix(0, 0), None);
        assert_eq!(dos_to_unix(0, (13 << 5) | 1), None);
    }

    /// 本地换算与 UTC 换算的差必须是整小时级别的时区偏移（±14h 内）。
    #[test]
    fn local_matches_utc_within_tz_offset() {
        // 2024-01-02 15:30:00
        let date = ((2024 - 1980) << 9) | (1 << 5) | 2;
        let time = (15 << 11) | (30 << 5);
        let local = dos_to_unix(time, date).unwrap();
        let utc = utc_to_unix(2024, 1, 2, 15, 30, 0);
        assert!((local - utc).abs() <= 14 * 3600, "local={local} utc={utc}");
        assert_eq!(utc, 1_704_209_400);
    }

    /// 同一小时内不同分秒走缓存，结果必须与逐个直接换算一致。
    #[test]
    fn hour_cache_matches_direct() {
        let date = ((2024 - 1980) << 9) | (3 << 5) | 31;
        for (m, s) in [(0u16, 0u16), (5, 10), (59, 29)] {
            let time = (2 << 11) | (m << 5) | s;
            let cached = dos_to_unix(time, date).unwrap();
            let direct = local_to_unix(2024, 3, 31, 2, m as u32, (s * 2) as u32).unwrap();
            assert_eq!(cached, direct, "m={m} s={s}");
        }
    }

    /// FILETIME 已知样本：2024-01-02 07:30:00 UTC。
    #[test]
    fn filetime_known_value() {
        let unix = utc_to_unix(2024, 1, 2, 7, 30, 0);
        let ft = (unix + FILETIME_EPOCH_DIFF) as u64 * 10_000_000;
        assert_eq!(filetime_to_unix(ft), Some(unix));
    }
}
