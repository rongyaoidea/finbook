//! 从其他软件（金蝶 / 用友 / Excel 导出）导入数据
//!
//! 提供两种 CSV 通用导入，复用账套既有校验保证数据一致：
//! - [`import_begin`]：期初余额表（科目编码, 方向, 金额）
//! - [`import_vouchers`]：凭证（日期, 凭证字, 摘要, 科目编码, 借方, 贷方）
//!
//! 分隔符自动识别逗号 / 制表符 / 分号，支持引号包裹。

use fincore::{AuxRef, Entry, Money, Period, Voucher, VoucherSource, VoucherStatus};

use crate::balances::{self, BeginRow};
use crate::vouchers;
use crate::{Db, DbResult};

/// 解析一行 CSV：自动识别 `,` `\t` `;`，支持 `"` 引号
pub fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_q && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_q = !in_q;
                }
            }
            ',' | '\t' | ';' if !in_q => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

fn parse_money(s: &str) -> Money {
    // 去掉千分位逗号与货币符号，兼容 "1,234.56" / "¥1,234.56"
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    Money::parse(&cleaned).unwrap_or(Money::ZERO)
}

fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '-').collect();
    chrono::NaiveDate::parse_from_str(&cleaned, "%Y-%m-%d")
        .ok()
        .or_else(|| chrono::NaiveDate::parse_from_str(&cleaned, "%Y/%m/%d").ok())
        .or_else(|| {
            if cleaned.len() == 8 && cleaned.chars().all(|c| c.is_ascii_digit()) {
                chrono::NaiveDate::parse_from_str(&cleaned, "%Y%m%d").ok()
            } else {
                None
            }
        })
}

/// 解析方向词："借" / "debit" / "d" / 正数 → 借；"贷" / "credit" / "c" / 负数 → 贷
fn parse_direction(s: &str, signed: Money) -> fincore::Direction {
    let t = s.trim().to_lowercase();
    match t.as_str() {
        "借" | "debit" | "d" | "借方" => fincore::Direction::Debit,
        "贷" | "credit" | "c" | "贷方" => fincore::Direction::Credit,
        _ => {
            if signed.is_negative() {
                fincore::Direction::Credit
            } else {
                fincore::Direction::Debit
            }
        }
    }
}

/// 导入结果：成功条数 + 警告列表（逐行失败不中断，全部尝试）
#[derive(Clone, Debug)]
pub struct ImportResult {
    pub ok: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
}

/// 预检出的缺失科目（供前端展示、让用户选择映射或忽略）
#[derive(Clone, Debug)]
pub struct MissingAccount {
    /// 源文件里的科目编码（如 "1002"）
    pub code: String,
    /// 对应名称（若有）
    pub name: String,
    /// 出现次数（用于排序提示）
    pub count: usize,
}

/// 提取文件中引用的所有科目编码（去重、带次数），供预检使用
fn collect_codes(text: &str, first_col_is_code: bool) -> Vec<String> {
    let mut map: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let f = split_csv(line);
        if f.is_empty() {
            continue;
        }
        let col = if first_col_is_code { 0 } else { 3 };
        let code = f.get(col).unwrap_or(&String::new()).trim().to_string();
        if !code.is_empty() && code.chars().any(|c| c.is_ascii_digit()) {
            *map.entry(code).or_insert(0) += 1;
        }
    }
    let mut out: Vec<String> = map.keys().cloned().collect();
    out.sort();
    out
}

/// 预检：找出文件中引用但账套里不存在的科目（供用户选择映射或忽略）
///
/// - `first_col_is_code = true`：期初余额表（第 1 列是科目）
/// - `first_col_is_code = false`：凭证（第 4 列是科目）
pub fn analyze_missing(
    db: &Db,
    text: &str,
    first_col_is_code: bool,
) -> DbResult<Vec<MissingAccount>> {
    let chart = crate::accounts::chart(db)?;
    let codes = collect_codes(text, first_col_is_code);
    let mut out = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for code in &codes {
        *seen.entry(code.clone()).or_insert(0) += 1;
    }
    for code in &codes {
        if chart.get(code).is_none() {
            out.push(MissingAccount {
                code: code.clone(),
                name: String::new(),
                count: seen.get(code).copied().unwrap_or(0),
            });
        }
    }
    Ok(out)
}

/// 执行导入：把源科目编码按 `mapping`（源编码 → 目标编码）替换后再写入。
/// 不在 mapping 里的缺失科目会跳过并警告；已存在科目不受影响。
pub fn apply_mapping(code: &str, mapping: &std::collections::HashMap<String, String>) -> String {
    mapping.get(code).cloned().unwrap_or_else(|| code.to_string())
}

/// 按科目表的辅助核算要求补齐分录的必填辅助字段（导入 CSV 常省略，需自动填默认值）
fn fill_required(db: &Db, e: &mut Entry) {
    let Ok(chart) = crate::accounts::chart(db) else {
        return;
    };
    let Some(a) = chart.get(&e.account_code) else {
        return;
    };
    for k in a.aux.list() {
        if e.aux.get(k).is_some() {
            continue;
        }
        let v = match k {
            fincore::AuxKind::Bank => "B01",
            fincore::AuxKind::Customer => "C01",
            fincore::AuxKind::Supplier => "S01",
            fincore::AuxKind::Item => "I01",
            fincore::AuxKind::Dept => "D01",
            fincore::AuxKind::Employee => "E01",
            fincore::AuxKind::Project => "P01",
            fincore::AuxKind::CashFlow => continue,
        };
        e.aux.set(k, Some(v.to_string()));
    }
    if a.has_qty && e.qty.is_none() {
        e.qty = Some(Money::ONE);
        e.price = Some(if e.debit.is_positive() { e.debit } else { e.credit });
    }
}

/// 导入期初余额表
///
/// CSV 列：`科目编码, 方向(借/贷), 金额`；方向省略时按金额正负推断。
/// 金额语义为"启用期初余额"（正 = 借，负 = 贷）。
/// 科目需为末级科目；非末级或不存在（且未在 `mapping` 中映射）则跳过并警告。
pub fn import_begin(
    db: &Db,
    text: &str,
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
) -> DbResult<ImportResult> {
    let chart = crate::accounts::chart(db)?;
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let f = split_csv(line);
        // 表头探测：第一列不像科目编码（非 4/6/8 位数字）则跳过
        let code = f.first().unwrap_or(&String::new()).trim().to_string();
        if code.is_empty() {
            continue;
        }
        if i == 0 && !code.chars().all(|c| c.is_ascii_digit()) {
            continue; // 表头
        }
        if f.len() < 2 {
            res.warnings.push(format!("第 {} 行：列数不足，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        // 源科目 → 目标科目（用户映射）
        let src_code = code.clone();
        let code = apply_mapping(&src_code, mapping);
        let amt = parse_money(f.get(2).unwrap_or(&String::new()));
        let dir = parse_direction(f.get(1).unwrap_or(&String::new()), amt);
        let signed = if dir == fincore::Direction::Debit {
            amt.abs()
        } else {
            -amt.abs()
        };
        // 校验科目
        match chart.get(&code) {
            Some(_) if chart.is_leaf(&code) => {
                balances::upsert_begin(
                    db,
                    &BeginRow {
                        id: 0,
                        account_code: code.clone(),
                        aux: AuxRef::default(),
                        year_begin: signed,
                        debit_accum: Money::ZERO,
                        credit_accum: Money::ZERO,
                        qty_begin: None,
                    },
                )?;
                res.ok += 1;
            }
            Some(_) => {
                res.warnings.push(format!(
                    "第 {} 行：科目 {code}（源 {src_code}）非末级，不能记期初，已跳过",
                    i + 1
                ));
                res.skipped += 1;
            }
            None => {
                res.warnings.push(format!(
                    "第 {} 行：科目 {code}（源 {src_code}）不存在，已跳过",
                    i + 1
                ));
                res.skipped += 1;
            }
        }
    }
    if res.ok > 0 {
        db.log(who, "导入", "导入期初余额", &format!("成功 {} 条", res.ok))?;
    }
    Ok(res)
}

/// 导入凭证
///
/// CSV 列：`日期, 凭证字, 摘要, 科目编码, 借方, 贷方`
/// - 凭证字省略时按"记"
/// - 同一期间内凭证号自动连续分配（按出现顺序）
/// - 借方/贷方必有一方非零；一行分录借贷必须平衡的凭证由校验把关
/// - 支持辅助核算简写列（可选第 7 列：客户编码）
/// - `mapping`：源科目 → 目标科目映射（用户在前端选择），缺失科目按映射替换
pub fn import_vouchers(
    db: &Db,
    period: Period,
    text: &str,
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
) -> DbResult<ImportResult> {
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    let mut pending: Option<Voucher> = None;

    // 收尾提交
    let mut flush = |v: &mut Option<Voucher>, res: &mut ImportResult, who: &str| -> DbResult<()> {
        if let Some(mut v) = v.take() {
            if v.entries.is_empty() {
                return Ok(());
            }
            if !v.balanced() {
                res.warnings.push(format!(
                    "凭证 {} 借贷不平衡，已跳过",
                    v.voucher_no()
                ));
                res.skipped += 1;
                return Ok(());
            }
            match vouchers::save(db, &mut v) {
                Ok(_) => res.ok += 1,
                Err(e) => {
                    res.warnings.push(format!("凭证 {} 导入失败：{e}", v.voucher_no()));
                    res.skipped += 1;
                }
            }
            let _ = who;
        }
        Ok(())
    };

    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let f = split_csv(line);
        if f.len() < 4 {
            continue;
        }
        // 表头探测：第一列不是日期则跳过
        if i == 0 && parse_date(&f[0]).is_none() {
            continue;
        }
        let date = match parse_date(&f[0]) {
            Some(d) => d,
            None => {
                res.warnings.push(format!("第 {} 行：日期无法识别，已跳过", i + 1));
                res.skipped += 1;
                continue;
            }
        };
        // 同一日期内按顺序分配凭证号；不同日期新起一张
        let new_voucher = match &pending {
            Some(v) => v.date != date,
            None => true,
        };
        if new_voucher {
            flush(&mut pending, &mut res, who)?;
            let no = vouchers::next_no(db, period, "记")?;
            let mut v = Voucher::new(period, date, "记".to_string(), no);
            v.source = VoucherSource::Import;
            v.status = VoucherStatus::Posted; // 记录即生效（与录入一致）
            v.posted_by = Some(who.to_string());
            pending = Some(v);
        }
        let v = pending.as_mut().unwrap();
        let src_code = f.get(3).unwrap_or(&String::new()).trim().to_string();
        let account_code = apply_mapping(&src_code, mapping);
        let summary = f.get(2).unwrap_or(&String::new()).trim().to_string();
        let debit = parse_money(f.get(4).unwrap_or(&String::new()));
        let credit = parse_money(f.get(5).unwrap_or(&String::new()));
        let mut entry = Entry::new(v.entries.len() as i32 + 1, account_code, summary);
        entry.debit = debit;
        entry.credit = credit;
        // 可选第 7 列：客户辅助核算
        if let Some(cust) = f.get(6) {
            let cust = cust.trim();
            if !cust.is_empty() {
                entry.aux.customer = Some(cust.to_string());
            }
        }
        // 未填的必填辅助核算按科目表补齐（如银行科目必须填银行账户）
        fill_required(db, &mut entry);
        v.entries.push(entry);
    }
    flush(&mut pending, &mut res, who)?;
    if res.ok > 0 {
        db.log(who, "导入", "导入凭证", &format!("成功 {} 张", res.ok))?;
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn csv_split_handles_quotes() {
        let f = split_csv(r#""a,1",b,"c,d""#);
        assert_eq!(f, vec!["a,1", "b", "c,d"]);
    }

    #[test]
    fn import_begin_basic() {
        let db = mem();
        let csv = "\u{feff}科目,方向,金额\n1001,借,10000\n100201,贷,2000\n600101,借,5000\n9999,借,1\n";
        let empty = std::collections::HashMap::new();
        let res = import_begin(&db, csv, "u1", &empty).unwrap();
        assert_eq!(res.ok, 3, "应导入 3 条：{:?}", res.warnings);
        assert_eq!(res.skipped, 1, "9999 不存在应跳过：{:?}", res.warnings);
        // 验证期初写入
        let rows = balances::list_begin(&db).unwrap();
        assert_eq!(rows.len(), 3);
        let cash = rows.iter().find(|r| r.account_code == "1001").unwrap();
        assert_eq!(cash.year_begin, Money::parse("10000").unwrap());
        let bank = rows.iter().find(|r| r.account_code == "100201").unwrap();
        assert_eq!(bank.year_begin, Money::parse("-2000").unwrap());
    }

    #[test]
    fn analyze_missing_lists_unknown_codes() {
        let db = mem();
        let csv = "\u{feff}科目,方向,金额\n1001,借,10000\n9999,借,1\n8888,借,2\n";
        let missing = analyze_missing(&db, csv, true).unwrap();
        let codes: Vec<&str> = missing.iter().map(|m| m.code.as_str()).collect();
        assert!(codes.contains(&"9999"), "应列出 9999：{codes:?}");
        assert!(codes.contains(&"8888"), "应列出 8888：{codes:?}");
        assert!(!codes.contains(&"1001"), "已存在科目不应列出：{codes:?}");
    }

    #[test]
    fn import_begin_with_mapping() {
        let db = mem();
        let csv = "9999,借,10000\n";
        let mut mapping = std::collections::HashMap::new();
        mapping.insert("9999".to_string(), "1001".to_string());
        let res = import_begin(&db, csv, "u1", &mapping).unwrap();
        assert_eq!(res.ok, 1, "映射后应导入：{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        assert_eq!(rows[0].account_code, "1001");
        assert_eq!(rows[0].year_begin, Money::parse("10000").unwrap());
    }

    #[test]
    fn import_vouchers_balanced() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let csv = "\u{feff}日期,凭证字,摘要,科目,借方,贷方,客户\n\
                  2026-01-05,记,收到货款,100201,0,1000,C01\n\
                  2026-01-05,记,收到货款,600101,1000,0,\n\
                  2026-01-08,记,提现,1001,500,0,\n\
                  2026-01-08,记,提现,100201,0,500,\n";
        let res = import_vouchers(&db, p, csv, "u1", &std::collections::HashMap::new()).unwrap();
        assert_eq!(res.ok, 2, "应导入 2 张：{:?}", res.warnings);
        assert_eq!(res.skipped, 0);
        // 验证已落库并参与汇总
        let all = vouchers::list(&db, &vouchers::VoucherQuery::period(p)).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].status, VoucherStatus::Posted);
    }

    #[test]
    fn import_vouchers_imbalanced_skipped() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let csv = "2026-01-05,记,不平,1001,100,0,\n2026-01-05,记,不平,600101,0,90,\n";
        let res = import_vouchers(&db, p, csv, "u1", &std::collections::HashMap::new()).unwrap();
        assert_eq!(res.ok, 0);
        assert_eq!(res.skipped, 1);
        assert!(res.warnings.iter().any(|w| w.contains("不平衡")));
    }
}