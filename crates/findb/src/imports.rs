//! 从其他软件（金蝶 / 用友 / Excel 导出）导入数据
//!
//! 提供两种通用导入，复用账套既有校验保证数据一致：
//! - [`import_begin`]：期初余额表（科目编码, 方向, 金额）
//! - [`import_vouchers`]：凭证（日期, 凭证字, 摘要, 科目编码, 借方, 贷方）
//!
//! 支持 CSV 文本粘贴与 Excel 文件直接读取（.xlsx/.xls/.ods）。
//! 分隔符自动识别逗号 / 制表符 / 分号，支持引号包裹。
//! 提供"来源模板"（金蝶 / 用友 / 通用），自动按对应列顺序解析。

use std::io::Cursor;

use fincore::{AuxRef, Entry, Money, Period, Voucher, VoucherSource};

use crate::balances::{self, BeginRow};
use crate::vouchers;
use crate::{Db, DbResult};

/// 导入来源模板：不同软件导出的列顺序不同，自动适配
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ImportTemplate {
    /// 通用 CSV（当前默认格式）
    #[default]
    Generic,
    /// 金蝶导出格式
    Kingdee,
    /// 用友导出格式
    Yonyou,
}

impl ImportTemplate {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "kingdee" | "金蝶" | "kd" => ImportTemplate::Kingdee,
            "yonyou" | "用友" | "yy" => ImportTemplate::Yonyou,
            _ => ImportTemplate::Generic,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ImportTemplate::Kingdee => "金蝶",
            ImportTemplate::Yonyou => "用友",
            ImportTemplate::Generic => "通用",
        }
    }

    pub const ALL: &'static [ImportTemplate] = &[
        ImportTemplate::Generic,
        ImportTemplate::Kingdee,
        ImportTemplate::Yonyou,
    ];
}

/// 期初余额表行：统一提取后的标准化行（与来源模板无关）
struct BeginLine {
    code: String,
    direction: String,
    amount: Money,
    /// 累计借方（金蝶/用友有此列，通用格式无）
    debit_accum: Option<Money>,
    /// 累计贷方
    credit_accum: Option<Money>,
}

/// 凭证行：统一提取后的标准化行
struct VoucherLine {
    date: chrono::NaiveDate,
    word: String,
    no: Option<i32>,
    summary: String,
    code: String,
    debit: Money,
    credit: Money,
    /// 辅助核算（客户编码，可选）
    customer: Option<String>,
}

/// 按模板从 CSV 行提取期初余额行
fn extract_begin_line(tmpl: ImportTemplate, f: &[String]) -> Option<BeginLine> {
    let code = f.first()?.trim().to_string();
    if code.is_empty() || !code.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    match tmpl {
        ImportTemplate::Generic => {
            // 科目编码, 方向, 金额
            if f.len() < 2 {
                return None;
            }
            let amt = parse_money(f.get(2).unwrap_or(&String::new()));
            Some(BeginLine {
                code,
                direction: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                amount: amt,
                debit_accum: None,
                credit_accum: None,
            })
        }
        ImportTemplate::Kingdee => {
            // 科目编码, 科目名称, 方向, 期初余额, 累计借方, 累计贷方
            if f.len() < 4 {
                return None;
            }
            let amt = parse_money(f.get(3).unwrap_or(&String::new()));
            Some(BeginLine {
                code,
                direction: f.get(2).unwrap_or(&String::new()).trim().to_string(),
                amount: amt,
                debit_accum: f.get(4).map(|s| parse_money(s)),
                credit_accum: f.get(5).map(|s| parse_money(s)),
            })
        }
        ImportTemplate::Yonyou => {
            // 科目编码, 科目名称, 期初借方, 期初贷方, 累计借方, 累计贷方
            if f.len() < 4 {
                return None;
            }
            let d = parse_money(f.get(2).unwrap_or(&String::new()));
            let c = parse_money(f.get(3).unwrap_or(&String::new()));
            let dir = if d > Money::ZERO { "借" } else { "贷" };
            let amt = if d > Money::ZERO { d } else { c };
            Some(BeginLine {
                code,
                direction: dir.to_string(),
                amount: amt,
                debit_accum: f.get(4).map(|s| parse_money(s)),
                credit_accum: f.get(5).map(|s| parse_money(s)),
            })
        }
    }
}

/// 按模板从 CSV 行提取凭证行
fn extract_voucher_line(tmpl: ImportTemplate, f: &[String]) -> Option<VoucherLine> {
    if f.len() < 4 {
        return None;
    }
    let date = parse_date(&f[0])?;
    match tmpl {
        ImportTemplate::Generic => {
            // 日期, 凭证字, 摘要, 科目编码, 借方, 贷方[, 客户]
            Some(VoucherLine {
                date,
                word: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                no: None,
                summary: f.get(2).unwrap_or(&String::new()).trim().to_string(),
                code: f.get(3).unwrap_or(&String::new()).trim().to_string(),
                debit: parse_money(f.get(4).unwrap_or(&String::new())),
                credit: parse_money(f.get(5).unwrap_or(&String::new())),
                customer: f.get(6).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            })
        }
        ImportTemplate::Kingdee => {
            // 日期, 凭证字, 凭证号, 摘要, 科目编码, 科目名称, 借方, 贷方
            Some(VoucherLine {
                date,
                word: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                no: f.get(2).and_then(|s| s.trim().parse().ok()),
                summary: f.get(3).unwrap_or(&String::new()).trim().to_string(),
                code: f.get(4).unwrap_or(&String::new()).trim().to_string(),
                debit: parse_money(f.get(6).unwrap_or(&String::new())),
                credit: parse_money(f.get(7).unwrap_or(&String::new())),
                customer: None,
            })
        }
        ImportTemplate::Yonyou => {
            // 日期, 凭证字号, 摘要, 科目编码, 借方, 贷方
            Some(VoucherLine {
                date,
                word: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                no: None,
                summary: f.get(2).unwrap_or(&String::new()).trim().to_string(),
                code: f.get(3).unwrap_or(&String::new()).trim().to_string(),
                debit: parse_money(f.get(4).unwrap_or(&String::new())),
                credit: parse_money(f.get(5).unwrap_or(&String::new())),
                customer: None,
            })
        }
    }
}

/// 读取 Excel 文件（.xlsx/.xls/.ods）第一个 sheet，返回所有行（每行是单元格列表）
pub fn read_xlsx(path: &std::path::Path) -> Result<Vec<Vec<String>>, fincore::FinError> {
    let bytes = std::fs::read(path)
        .map_err(|e| fincore::FinError::io(format!("读取文件失败：{e}")))?;
    read_xlsx_bytes(&bytes)
}

/// 从字节读取 Excel（.xlsx/.xls/.ods）第一个 sheet，返回所有行（每行是单元格列表）
///
/// Web 端可直接把上传的文件字节交给本函数，无需落盘。
pub fn read_xlsx_bytes(bytes: &[u8]) -> Result<Vec<Vec<String>>, fincore::FinError> {
    use calamine::{open_workbook_auto_from_rs, Data, Reader};
    let mut wb = open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|e| fincore::FinError::io(format!("打开 Excel 失败：{e}")))?;
    let sheet_name = wb
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| fincore::FinError::msg("Excel 无 sheet"))?;
    let range = wb
        .worksheet_range(&sheet_name)
        .map_err(|e| fincore::FinError::io(format!("读取 sheet 失败：{e}")))?;
    let mut rows = Vec::new();
    for row in range.rows() {
        let cells: Vec<String> = row
            .iter()
            .map(|c| match c {
                Data::String(s) => s.clone(),
                Data::Int(n) => n.to_string(),
                Data::Float(f) => format!("{f}"),
                Data::Bool(b) => b.to_string(),
                Data::DateTime(d) => d.to_string(),
                Data::DateTimeIso(s) => s.clone(),
                Data::DurationIso(s) => s.clone(),
                _ => String::new(),
            })
            .collect();
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    Ok(rows)
}

/// 把 Excel 行列表转成 CSV 文本（每行用逗号连接），复用 CSV 解析逻辑
pub fn xlsx_to_csv_text(rows: &[Vec<String>]) -> String {
    rows.iter()
        .map(|r| {
            r.iter()
                .map(|c| {
                    if c.contains(',') || c.contains('"') {
                        format!("\"{}\"", c.replace('"', "\"\""))
                    } else {
                        c.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

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
    // 先规范化 Unicode 字符：全角数字、Unicode 负号（U+2212）、全角括号等
    let normalized: String = s
        .chars()
        .map(|c| match c {
            '\u{ff0d}' => '-', // 全角减号
            '\u{2212}' => '-', // Unicode 减号
            '\u{FF00}'..='\u{FF60}' => (c as u32 - 0xFFEE) as u8 as char, // 全角数字转半角
            _ => c,
        })
        .collect();
    // 去掉千分位逗号与货币符号，兼容 "1,234.56" / "¥1,234.56" / "−100"
    let cleaned: String = normalized
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

/// 把 CSV 文本拆成行列表（每行是单元格列表），供 Excel / 文本统一处理
fn text_to_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let f = split_csv(line);
        if !f.is_empty() {
            rows.push(f);
        }
    }
    rows
}

/// 提取文件中引用的所有科目编码（去重、带次数），供预检使用
fn collect_codes(rows: &[Vec<String>], tmpl: ImportTemplate, is_begin: bool) -> Vec<String> {
    let mut map: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for f in rows {
        // 科目列位置随来源模板不同：期初恒为第 1 列；凭证通用/用友为第 4 列，金蝶为第 5 列
        let col = if is_begin {
            0
        } else {
            match tmpl {
                ImportTemplate::Kingdee => 4,
                _ => 3,
            }
        };
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
/// - `tmpl`：来源模板（决定凭证的科目列位置）
/// - `is_begin = true`：期初余额表（第 1 列是科目）；`false`：凭证
pub fn analyze_missing(
    db: &Db,
    text: &str,
    tmpl: ImportTemplate,
    is_begin: bool,
) -> DbResult<Vec<MissingAccount>> {
    let chart = crate::accounts::chart(db)?;
    let rows = text_to_rows(text);
    let codes = collect_codes(&rows, tmpl, is_begin);
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

/// 导入期初余额表（CSV 文本）
///
/// - 通用列：`科目编码, 方向(借/贷), 金额`；方向省略时按金额正负推断
/// - 金蝶列：`科目编码, 科目名称, 方向, 期初余额, 累计借方, 累计贷方`
/// - 用友列：`科目编码, 科目名称, 期初借方, 期初贷方, 累计借方, 累计贷方`
///
/// 金额语义为"启用期初余额"（正 = 借，负 = 贷）。
/// 科目需为末级科目；非末级或不存在（且未在 `mapping` 中映射）则跳过并警告。
pub fn import_begin(
    db: &Db,
    text: &str,
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let rows = text_to_rows(text);
    import_begin_rows(db, &rows, who, mapping, tmpl)
}

/// 导入期初余额表（Excel 文件字节，.xlsx/.xls/.ods）
pub fn import_begin_bytes(
    db: &Db,
    bytes: &[u8],
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let rows = read_xlsx_bytes(bytes)?;
    import_begin_rows(db, &rows, who, mapping, tmpl)
}

/// 导入期初余额表核心：按模板解析每一行（文本与 Excel 共用）
fn import_begin_rows(
    db: &Db,
    rows: &[Vec<String>],
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let chart = crate::accounts::chart(db)?;
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    for (i, f) in rows.iter().enumerate() {
        let Some(line) = extract_begin_line(tmpl, f) else {
            continue; // 表头 / 说明行
        };
        // 源科目 → 目标科目（用户映射）
        let src_code = line.code.clone();
        let code = apply_mapping(&src_code, mapping);
        let amt = line.amount;
        let dir = parse_direction(&line.direction, amt);
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
                        debit_accum: line.debit_accum.unwrap_or(Money::ZERO),
                        credit_accum: line.credit_accum.unwrap_or(Money::ZERO),
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

/// 导入凭证（CSV 文本）
///
/// - 通用列：`日期, 凭证字, 摘要, 科目编码, 借方, 贷方[, 客户编码]`
/// - 金蝶列：`日期, 凭证字, 凭证号, 摘要, 科目编码, 科目名称, 借方, 贷方`
/// - 用友列：`日期, 凭证字号, 摘要, 科目编码, 借方, 贷方`
///
/// - 凭证字省略时按"记"
/// - 同一期间内凭证号自动连续分配（按出现顺序）
/// - 借方/贷方必有一方非零；一行分录借贷必须平衡的凭证由校验把关
/// - 支持辅助核算简写列（通用模板第 7 列：客户编码）
/// - `mapping`：源科目 → 目标科目映射（用户在前端选择），缺失科目按映射替换
pub fn import_vouchers(
    db: &Db,
    period: Period,
    text: &str,
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let rows = text_to_rows(text);
    import_vouchers_rows(db, period, &rows, who, mapping, tmpl)
}

/// 导入凭证（Excel 文件字节，.xlsx/.xls/.ods）
pub fn import_vouchers_bytes(
    db: &Db,
    period: Period,
    bytes: &[u8],
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let rows = read_xlsx_bytes(bytes)?;
    import_vouchers_rows(db, period, &rows, who, mapping, tmpl)
}

/// 导入凭证核心：按模板解析每一行（文本与 Excel 共用）
fn import_vouchers_rows(
    db: &Db,
    period: Period,
    rows: &[Vec<String>],
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    let mut pending: Option<Voucher> = None;
    // 当前待提交凭证对应的源凭证号（金蝶模板有；通用/用友无，恒为 None）
    let mut pending_no: Option<i32> = None;

    // 收尾提交
    let flush = |v: &mut Option<Voucher>, res: &mut ImportResult, who: &str| -> DbResult<()> {
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

    for f in rows.iter() {
        let Some(line) = extract_voucher_line(tmpl, f) else {
            continue; // 表头 / 说明行
        };
        let date = line.date;
        // 同一（日期 + 凭证号）内共一张凭证：金蝶模板带凭证号，同日期多张凭证号应分开；
        // 通用/用友无凭证号，按日期分。
        let new_voucher = match &pending {
            Some(v) => {
                let date_changed = v.date != date;
                let no_changed = tmpl == ImportTemplate::Kingdee && pending_no != line.no;
                date_changed || no_changed
            }
            None => true,
        };
        if new_voucher {
            flush(&mut pending, &mut res, who)?;
            pending_no = line.no;
            let word = if line.word.trim().is_empty() { "记" } else { line.word.trim() };
            let no = vouchers::next_no(db, period, word)?;
            let mut v = Voucher::new(period, date, word.to_string(), no);
            v.source = VoucherSource::Import; // 导入后为未记账，与录入一致，核对后在期末处理批量记账
            pending = Some(v);
        }
        let v = pending.as_mut().unwrap();
        let src_code = line.code.clone();
        let account_code = apply_mapping(&src_code, mapping);
        let mut entry = Entry::new(v.entries.len() as i32 + 1, account_code, line.summary);
        entry.debit = line.debit;
        entry.credit = line.credit;
        // 可选客户辅助核算
        if let Some(cust) = &line.customer {
            entry.aux.customer = Some(cust.clone());
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
    use fincore::VoucherStatus;

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
        let res = import_begin(&db, csv, "u1", &empty, ImportTemplate::Generic).unwrap();
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
        let missing = analyze_missing(&db, csv, ImportTemplate::Generic, true).unwrap();
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
        let res = import_begin(&db, csv, "u1", &mapping, ImportTemplate::Generic).unwrap();
        assert_eq!(res.ok, 1, "映射后应导入：{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        assert_eq!(rows[0].account_code, "1001");
        assert_eq!(rows[0].year_begin, Money::parse("10000").unwrap());
    }

    #[test]
    fn import_begin_kingdee_template() {
        let db = mem();
        // 金蝶：科目编码, 科目名称, 方向, 期初余额, 累计借方, 累计贷方
        let csv = "\u{feff}科目编码,科目名称,方向,期初余额,累计借方,累计贷方\n\
                   1001,库存现金,借,10000,50000,30000\n\
                   100201,银行存款-工行,贷,2000,0,2000\n\
                   600101,主营业务收入,贷,0,0,0\n";
        let res =
            import_begin(&db, csv, "u1", &std::collections::HashMap::new(), ImportTemplate::Kingdee)
                .unwrap();
        assert_eq!(res.ok, 3, "金蝶模板应导入 3 条：{:?}", res.warnings);
        assert_eq!(res.skipped, 0, "全部存在：{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        let cash = rows.iter().find(|r| r.account_code == "1001").unwrap();
        assert_eq!(cash.year_begin, Money::parse("10000").unwrap());
        assert_eq!(cash.debit_accum, Money::parse("50000").unwrap());
        assert_eq!(cash.credit_accum, Money::parse("30000").unwrap());
        let bank = rows.iter().find(|r| r.account_code == "100201").unwrap();
        assert_eq!(bank.year_begin, Money::parse("-2000").unwrap());
    }

    #[test]
    fn import_begin_yonyou_template() {
        let db = mem();
        // 用友：科目编码, 科目名称, 期初借方, 期初贷方, 累计借方, 累计贷方
        let csv = "\u{feff}科目编码,科目名称,期初借方,期初贷方,累计借方,累计贷方\n\
                   1001,库存现金,10000,0,50000,0\n\
                   100201,银行存款-工行,0,2000,0,2000\n";
        let res =
            import_begin(&db, csv, "u1", &std::collections::HashMap::new(), ImportTemplate::Yonyou)
                .unwrap();
        assert_eq!(res.ok, 2, "用友模板应导入 2 条：{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        let cash = rows.iter().find(|r| r.account_code == "1001").unwrap();
        assert_eq!(cash.year_begin, Money::parse("10000").unwrap());
        assert_eq!(cash.debit_accum, Money::parse("50000").unwrap());
        let bank = rows.iter().find(|r| r.account_code == "100201").unwrap();
        assert_eq!(bank.year_begin, Money::parse("-2000").unwrap());
        assert_eq!(bank.credit_accum, Money::parse("2000").unwrap());
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
        let res = import_vouchers(
            &db,
            p,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Generic,
        )
        .unwrap();
        assert_eq!(res.ok, 2, "应导入 2 张：{:?}", res.warnings);
        assert_eq!(res.skipped, 0);
        // 验证已落库并参与汇总（导入后为未记账，与录入一致）
        let all = vouchers::list(&db, &vouchers::VoucherQuery::period(p)).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].status, VoucherStatus::Draft);
    }

    #[test]
    fn import_vouchers_kingdee_template() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 金蝶：日期, 凭证字, 凭证号, 摘要, 科目编码, 科目名称, 借方, 贷方
        let csv = "\u{feff}日期,凭证字,凭证号,摘要,科目编码,科目名称,借方,贷方\n\
                   2026-01-05,记,1,收到货款,100201,银行存款-工行,0,1000\n\
                   2026-01-05,记,1,收到货款,600101,主营业务收入,1000,0\n\
                   2026-01-05,记,2,提现,1001,库存现金,500,0\n\
                   2026-01-05,记,2,提现,100201,银行存款-工行,0,500\n";
        let res = import_vouchers(
            &db,
            p,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Kingdee,
        )
        .unwrap();
        assert_eq!(res.ok, 2, "金蝶模板应导入 2 张：{:?}", res.warnings);
        let all = vouchers::list(&db, &vouchers::VoucherQuery::period(p)).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|v| v.entries.is_empty() || v.entries.len() >= 2));
    }

    #[test]
    fn read_xlsx_fixture_parses_rows() {
        // fixture：用友模板期初表（科目编码,科目名称,期初借方,期初贷方,累计借方,累计贷方）
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/yonyou_begin.xlsx");
        let rows = read_xlsx(std::path::Path::new(path)).expect("读取 fixture xlsx 应成功");
        assert_eq!(rows.len(), 3, "应为表头 + 2 行数据");
        assert_eq!(rows[0][0], "科目编码");
        assert_eq!(rows[1][0], "1001");
        assert_eq!(rows[1][2], "10000");
        assert_eq!(rows[2][0], "100201");
        assert_eq!(rows[2][3], "2000");
    }

    #[test]
    fn import_begin_from_excel_bytes() {
        let db = mem();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/yonyou_begin.xlsx");
        let bytes = std::fs::read(path).unwrap();
        let res = import_begin_bytes(&db, &bytes, "u1", &std::collections::HashMap::new(), ImportTemplate::Yonyou)
            .unwrap();
        assert_eq!(res.ok, 2, "Excel 用友模板应导入 2 条：{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        let cash = rows.iter().find(|r| r.account_code == "1001").unwrap();
        assert_eq!(cash.year_begin, Money::parse("10000").unwrap());
        assert_eq!(cash.debit_accum, Money::parse("50000").unwrap());
        let bank = rows.iter().find(|r| r.account_code == "100201").unwrap();
        assert_eq!(bank.year_begin, Money::parse("-2000").unwrap());
        assert_eq!(bank.credit_accum, Money::parse("2000").unwrap());
    }
}