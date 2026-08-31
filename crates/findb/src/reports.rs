//! 报表模板与现金流量取数

use std::collections::HashMap;

use fincore::report::cashflow::{
    build_cash_flow, default_cash_flow_items, CashFlowDirection, CashFlowGroup, CashFlowItem,
    CashFlowStatement, ItemAmounts,
};
use fincore::report::{balance_sheet, income, ReportDef};
use fincore::Money;

use rusqlite::OptionalExtension;

use crate::balances::{BalanceQuery, BalanceSnapshot};
use crate::{accounts, read_money, Db, DbResult};

/// 写入内置报表模板（已存在则不覆盖，保留用户自定义）
pub fn ensure_defaults(db: &Db) -> DbResult<()> {
    for def in [
        balance_sheet::balance_sheet_def(),
        income::income_statement_def(),
    ] {
        let exists: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM report_def WHERE key=?1",
            rusqlite::params![def.key],
            |r| r.get(0),
        )?;
        if exists == 0 {
            save_def(db, &def)?;
        }
    }
    Ok(())
}

/// 恢复内置现金流量项目（覆盖同名编码）
pub fn reset_cash_flow_items(db: &Db) -> DbResult<usize> {
    let items = default_cash_flow_items();
    let mut n = 0usize;
    for it in &items {
        let g = match it.group {
            CashFlowGroup::Operating => "operating",
            CashFlowGroup::Investing => "investing",
            CashFlowGroup::Financing => "financing",
        };
        let d = match it.dir {
            CashFlowDirection::In => "in",
            CashFlowDirection::Out => "out",
        };
        db.conn().execute(
            "INSERT OR REPLACE INTO cash_flow_item(code,name,grp,dir,disabled)
             VALUES(?1,?2,?3,?4,0)",
            rusqlite::params![it.code, it.name, g, d],
        )?;
        n += 1;
    }
    Ok(n)
}

pub fn list_defs(db: &Db) -> DbResult<Vec<ReportDef>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT key,name,columns_json,lines_json FROM report_def ORDER BY key")?;
    let rows = stmt
        .query_map([], |r| {
            let key: String = r.get(0)?;
            let name: String = r.get(1)?;
            let cols: String = r.get(2)?;
            let lines: String = r.get(3)?;
            Ok(ReportDef {
                key,
                name,
                columns: serde_json::from_str(&cols).unwrap_or_default(),
                lines: serde_json::from_str(&lines).unwrap_or_default(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_def(db: &Db, key: &str) -> DbResult<Option<ReportDef>> {
    db.conn()
        .query_row(
            "SELECT key,name,columns_json,lines_json FROM report_def WHERE key=?1",
            rusqlite::params![key],
            |r| {
                let cols: String = r.get(2)?;
                let lines: String = r.get(3)?;
                Ok(ReportDef {
                    key: r.get(0)?,
                    name: r.get(1)?,
                    columns: serde_json::from_str(&cols).unwrap_or_default(),
                    lines: serde_json::from_str(&lines).unwrap_or_default(),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub fn save_def(db: &Db, def: &ReportDef) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO report_def(key,name,columns_json,lines_json) VALUES(?1,?2,?3,?4)
         ON CONFLICT(key) DO UPDATE SET name=excluded.name,
             columns_json=excluded.columns_json, lines_json=excluded.lines_json",
        rusqlite::params![
            def.key,
            def.name,
            serde_json::to_string(&def.columns)?,
            serde_json::to_string(&def.lines)?
        ],
    )?;
    Ok(())
}

/// 恢复内置模板
pub fn reset_def(db: &Db, key: &str) -> DbResult<()> {
    let def = match key {
        "balance_sheet" => balance_sheet::balance_sheet_def(),
        "income_statement" => income::income_statement_def(),
        _ => return Err(fincore::FinError::msg(format!("未知报表 {key}")).into()),
    };
    save_def(db, &def)
}

// ---------------------------------------------------------------------------
// 现金流量
// ---------------------------------------------------------------------------

pub fn cash_flow_items(db: &Db) -> DbResult<Vec<CashFlowItem>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT code,name,grp,dir,disabled FROM cash_flow_item ORDER BY code")?;
    let rows = stmt
        .query_map([], |r| {
            let g: String = r.get(2)?;
            let d: String = r.get(3)?;
            Ok(CashFlowItem {
                code: r.get(0)?,
                name: r.get(1)?,
                group: match g.as_str() {
                    "investing" => CashFlowGroup::Investing,
                    "financing" => CashFlowGroup::Financing,
                    _ => CashFlowGroup::Operating,
                },
                dir: match d.as_str() {
                    "out" => CashFlowDirection::Out,
                    _ => CashFlowDirection::In,
                },
                disabled: r.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 现金及现金等价物科目（末级）
pub fn cash_accounts(db: &Db) -> DbResult<Vec<String>> {
    let chart = accounts::chart(db)?;
    Ok(chart
        .all()
        .into_iter()
        .filter(|a| (a.is_cash || a.is_bank) && chart.is_leaf(&a.code))
        .map(|a| a.code.clone())
        .collect())
}

/// 按现金流量项目汇总发生额（只统计现金/银行科目的已记账分录）
pub fn cash_flow_amounts(db: &Db, from: fincore::Period, to: fincore::Period) -> DbResult<ItemAmounts> {
    let codes = cash_accounts(db)?;
    if codes.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = codes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT e.cf_item, e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON e.voucher_id=v.id
         WHERE v.status='posted' AND e.period BETWEEN ?1 AND ?2
           AND e.account_code IN ({placeholders})"
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
        Box::new(from.ymm()),
        Box::new(to.ymm()),
    ];
    for c in &codes {
        params.push(Box::new(c.clone()));
    }
    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(refs.as_slice())?;

    let mut amt = ItemAmounts::new();
    let mut unassigned_net = Money::ZERO;
    while let Some(r) = rows.next()? {
        let item: Option<String> = r.get(0)?;
        let d = read_money(r, 1)?;
        let c = read_money(r, 2)?;
        match item {
            Some(code) if !code.is_empty() => {
                let e = amt.entry(code).or_insert((Money::ZERO, Money::ZERO));
                e.0 += d;
                e.1 += c;
            }
            _ => {
                unassigned_net += d - c;
            }
        }
    }
    // 未标注项目的金额也要参与勾稽，用内部键记录下来
    if !unassigned_net.is_zero() {
        amt.insert("__unassigned__".to_string(), (unassigned_net, Money::ZERO));
    }
    Ok(amt)
}

/// 现金及现金等价物的期初 / 期末余额
pub fn cash_begin_end(db: &Db, from: fincore::Period, to: fincore::Period) -> DbResult<(Money, Money)> {
    let codes = cash_accounts(db)?;
    let snap = BalanceSnapshot::load(
        db,
        &BalanceQuery {
            from,
            to,
            ..BalanceQuery::period(from)
        },
    )?;
    let mut begin = Money::ZERO;
    let mut end = Money::ZERO;
    for c in codes {
        let r = snap.for_account(&c, None);
        begin += r.begin;
        end += r.end();
    }
    Ok((begin, end))
}

/// 生成现金流量表
pub fn cash_flow_statement(
    db: &Db,
    from: fincore::Period,
    to: fincore::Period,
) -> DbResult<CashFlowStatement> {
    let mut amounts = cash_flow_amounts(db, from, to)?;
    let unassigned = amounts
        .remove("__unassigned__")
        .map(|(net, _)| net)
        .unwrap_or(Money::ZERO);
    let (begin, end) = cash_begin_end(db, from, to)?;
    let items = cash_flow_items(db)?;
    let items = if items.is_empty() {
        default_cash_flow_items()
    } else {
        items
    };
    Ok(build_cash_flow(&items, &amounts, begin, end, unassigned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use crate::vouchers;
    use chrono::NaiveDate;
    use fincore::{AuxRef, Entry, Period, Voucher};

    #[test]
    fn default_defs_seeded() {
        let db = mem();
        let bs = get_def(&db, "balance_sheet").unwrap().unwrap();
        assert_eq!(bs.name, "资产负债表");
        let inc = get_def(&db, "income_statement").unwrap().unwrap();
        assert_eq!(inc.name, "利润表");
        assert!(!list_defs(&db).unwrap().is_empty());
    }

    #[test]
    fn def_roundtrip() {
        let db = mem();
        let mut def = get_def(&db, "income_statement").unwrap().unwrap();
        def.name = "自定义利润表".into();
        save_def(&db, &def).unwrap();
        assert_eq!(get_def(&db, "income_statement").unwrap().unwrap().name, "自定义利润表");
        reset_def(&db, "income_statement").unwrap();
        assert_eq!(get_def(&db, "income_statement").unwrap().unwrap().name, "利润表");
    }

    fn cash_voucher(db: &Db, p: Period, day: u32, entries: Vec<(&str, &str, &str, Option<&str>)>) {
        let d = NaiveDate::from_ymd_opt(p.year(), p.month(), day).unwrap();
        let mut v = Voucher::new(p, d, "记", vouchers::next_no(db, p, "记").unwrap());
        v.prepared_by = "张三".to_string();
        let mut i = 0;
        for (code, side, amt, cf) in entries {
            i += 1;
            let m = Money::parse(amt).unwrap();
            let mut e = Entry::new(i, code, "现金流测试");
            if side == "借" {
                e.debit = m;
            } else {
                e.credit = m;
            }
            if let Some(c) = cf {
                e.aux.cash_flow = Some(c.to_string());
            }
            fill_required(db, &mut e);
            v.push_entry(e);
        }
        let id = vouchers::save(db, &mut v).unwrap();
        vouchers::audit(db, id, "李四").unwrap();
        vouchers::post(db, id, "王五").unwrap();
    }


    /// 按科目表补齐辅助核算与数量（save 会强校验，测试分录必须先合规）
    fn fill_required(db: &Db, e: &mut Entry) {
        let Ok(chart) = crate::accounts::chart(db) else {
            return;
        };
        let Some(a) = chart.get(&e.account_code).cloned() else {
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
            let amt = if e.debit.is_positive() { e.debit } else { e.credit };
            e.qty = Some(Money::ONE);
            e.price = Some(amt);
        }
    }

    #[test]
    fn cash_flow_ties() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 销售收款 10 万（流入 0101）
        cash_voucher(
            &db,
            p,
            5,
            vec![
                ("1001", "借", "100000", Some("0101")),
                ("6001", "贷", "100000", None),
            ],
        );
        // 采购付款 4 万（流出 0104）
        cash_voucher(
            &db,
            p,
            6,
            vec![
                ("1405", "借", "40000", None),
                ("1001", "贷", "40000", Some("0104")),
            ],
        );

        let stmt = cash_flow_statement(&db, p, p).unwrap();
        assert_eq!(stmt.operating_net, Money::parse("60000").unwrap());
        assert_eq!(stmt.net_increase, Money::parse("60000").unwrap());
        assert_eq!(stmt.begin_cash, Money::ZERO);
        assert_eq!(stmt.end_cash, Money::parse("60000").unwrap());
        assert!(stmt.ties(), "现金流量表净增加额应与货币资金变动勾稽");
        assert_eq!(stmt.unassigned, Money::ZERO);
    }

    #[test]
    fn unassigned_reported() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 不标注现金流量项目
        cash_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "5000", None), ("6001", "贷", "5000", None)],
        );
        let stmt = cash_flow_statement(&db, p, p).unwrap();
        assert_eq!(stmt.unassigned, Money::parse("5000").unwrap());
        assert!(stmt.ties());
    }

    #[test]
    fn cash_accounts_found() {
        let db = mem();
        let codes = cash_accounts(&db).unwrap();
        assert!(codes.contains(&"1001".to_string()));
        assert!(codes.contains(&"100201".to_string()));
        assert!(!codes.contains(&"1002".to_string()), "非末级不应计入");
    }

    #[test]
    fn items_seeded() {
        let db = mem();
        let items = cash_flow_items(&db).unwrap();
        assert!(items.len() >= 15);
        assert!(items.iter().any(|i| i.code == "0101"));
    }

    #[test]
    fn aux_unused_placeholder() {
        // 保持 AuxRef 引用，避免未使用告警影响整洁
        let _ = AuxRef::default().key();
    }
}

