//! 库存深度：序列号 / 多单位换算 / 账龄分析 / ABC 分析 / 组装拆卸 / 库存状态 / 调拨报表
//!
//! 对标金蝶/用友库存管理。金额数量一律 TEXT 存储、Rust 侧 Decimal 累加。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

// ===========================================================================
// 序列号
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Serial {
    pub serial: String,
    pub item: String,
    pub batch_no: String,
    pub status: String, // in / out / scrapped
    pub in_date: String,
    pub out_date: Option<String>,
    pub memo: String,
}

/// 入库登记序列号（批量）
pub fn serial_in(db: &Db, item: &str, serials: &[String], batch_no: &str, date: NaiveDate) -> DbResult<usize> {
    let tx = db.write_tx()?;
    let mut n = 0;
    for s in serials {
        tx.execute(
            "INSERT INTO item_serial(serial,item,batch_no,status,in_date) VALUES(?1,?2,?3,'in',?4)
             ON CONFLICT(serial) DO UPDATE SET item=excluded.item, batch_no=excluded.batch_no, status='in', in_date=excluded.in_date",
            rusqlite::params![s, item, batch_no, date.format("%Y-%m-%d").to_string()],
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// 出库登记序列号：在库 → 已出库
pub fn serial_out(db: &Db, serials: &[String], date: NaiveDate) -> DbResult<usize> {
    let tx = db.write_tx()?;
    let mut n = 0;
    for s in serials {
        let cnt = tx.execute(
            "UPDATE item_serial SET status='out', out_date=?2 WHERE serial=?1 AND status='in'",
            rusqlite::params![s, date.format("%Y-%m-%d").to_string()],
        )?;
        n += cnt;
    }
    tx.commit()?;
    Ok(n)
}

/// 在库序列号清单
pub fn serial_list(db: &Db, item: &str) -> DbResult<Vec<Serial>> {
    let mut st = db.conn().prepare(
        "SELECT serial,item,batch_no,status,in_date,out_date,memo FROM item_serial WHERE item=?1 ORDER BY serial",
    )?;
    let rows = st
        .query_map([item], |r| {
            Ok(Serial {
                serial: r.get(0)?,
                item: r.get(1)?,
                batch_no: r.get(2)?,
                status: r.get(3)?,
                in_date: r.get(4)?,
                out_date: r.get(5)?,
                memo: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ===========================================================================
// 多单位换算
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ItemUnit {
    pub item: String,
    pub base_unit: String,
    pub alt_unit: String,
    /// 1 主单位 = factor 辅助单位
    pub factor: Money,
}

pub fn unit_get(db: &Db, item: &str) -> DbResult<Option<ItemUnit>> {
    db.conn()
        .query_row(
            "SELECT item,base_unit,alt_unit,factor FROM item_unit WHERE item=?1",
            [item],
            |r| {
                Ok(ItemUnit {
                    item: r.get(0)?,
                    base_unit: r.get(1)?,
                    alt_unit: r.get(2)?,
                    factor: m(&r.get::<_, String>(3)?),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub fn unit_set(db: &Db, u: &ItemUnit) -> DbResult<()> {
    if u.factor <= Money::ZERO {
        return Err(fincore::FinError::msg("换算系数必须大于 0").into());
    }
    db.conn().execute(
        "INSERT INTO item_unit(item,base_unit,alt_unit,factor) VALUES(?1,?2,?3,?4)
         ON CONFLICT(item) DO UPDATE SET base_unit=excluded.base_unit, alt_unit=excluded.alt_unit, factor=excluded.factor",
        rusqlite::params![u.item, u.base_unit, u.alt_unit, crate::exact_param(u.factor)],
    )?;
    Ok(())
}

/// 主单位数量 → 辅助单位数量
pub fn unit_to_alt(db: &Db, item: &str, base_qty: Money) -> DbResult<Money> {
    match unit_get(db, item)? {
        Some(u) => Ok(base_qty
            .checked_div(u.factor.inner())
            .expect("换算系数已校验 > 0（保存入口 inventory2.rs:115）")
            .round_dp(fincore::money::QTY_DP)),
        None => Ok(base_qty),
    }
}

// ===========================================================================
// 账龄分析 / ABC 分析
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize)]
pub struct InvAging {
    pub item: String,
    /// 最近一次入库日期
    pub last_in: Option<String>,
    /// 账龄天数（距今天）
    pub days: i64,
    /// 结存数量
    pub qty: Money,
    /// 结存金额
    pub amount: Money,
}

/// 库存账龄：按最近入库日期算账龄，按账龄降序
pub fn inv_aging(db: &Db, upto: Period) -> DbResult<Vec<InvAging>> {
    let items = crate::business::stock_items(db)?;
    let today = chrono::Local::now().date_naive();
    let mut out = Vec::new();
    for item in items {
        let st = crate::business::stock_state(db, &item, upto, fincore::engine::costing::CostMethod::MovingAverage)?;
        if st.qty.is_zero() {
            continue;
        }
        let last_in: Option<String> = db.conn().query_row(
            "SELECT biz_date FROM stock_move WHERE item=?1 AND qty>0 ORDER BY biz_date DESC LIMIT 1",
            [&item],
            |r| r.get(0),
        ).optional()?;
        let days = match &last_in {
            Some(d) => {
                let d = NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap_or(today);
                (today - d).num_days().max(0)
            }
            None => 0,
        };
        out.push(InvAging { item, last_in, days, qty: st.qty, amount: st.amount });
    }
    out.sort_by(|a, b| b.days.cmp(&a.days));
    Ok(out)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AbcRow {
    pub item: String,
    pub amount: Money,
    /// 累计金额占比（%）
    pub cum_pct: Money,
    /// A/B/C 分类
    pub class: String,
}

/// ABC 分析：按结存金额降序，累计占比 <=80% A、<=95% B、其余 C
pub fn abc_analysis(db: &Db, upto: Period) -> DbResult<Vec<AbcRow>> {
    let mut aging = inv_aging(db, upto)?;
    // 按结存金额降序（inv_aging 返回的是按账龄排序，这里重排）
    aging.sort_by(|a, b| b.amount.cmp(&a.amount));
    let total: Money = aging.iter().map(|a| a.amount).sum();
    let mut rows: Vec<AbcRow> = aging
        .iter()
        .map(|a| AbcRow { item: a.item.clone(), amount: a.amount, cum_pct: Money::ZERO, class: "C".into() })
        .collect();
    let mut cum = Money::ZERO;
    for r in rows.iter_mut() {
        cum += r.amount;
        let pct = if total.is_zero() {
            Money::ZERO
        } else {
            (cum * Money::from_i64(100))
                .checked_div(total.inner())
                .expect("total 已判非零")
                .round2()
        };
        r.cum_pct = pct;
        r.class = if pct <= Money::parse("80").unwrap() {
            "A"
        } else if pct <= Money::parse("95").unwrap() {
            "B"
        } else {
            "C"
        }
        .into();
    }
    Ok(rows)
}

// ===========================================================================
// 组装 / 拆卸 / 库存状态 / 调拨报表
// ===========================================================================

/// 组装：把多个子件组合成 1 个成品（子件出库、成品入库）
pub fn assemble(db: &Db, period: Period, date: NaiveDate, parent: &str, children: &[(String, Money)], memo: &str) -> DbResult<()> {
    use crate::business::{stock_insert, StockKind, StockMove};
    // 子件出库
    for (item, qty) in children {
        stock_insert(db, &StockMove {
            id: 0, period, biz_date: date, kind: StockKind::OtherOut,
            item: item.clone(), warehouse: String::new(), batch_no: String::new(),
            qty: qty.negated(), price: Money::ZERO, amount: Money::ZERO, voucher_id: None,
            memo: format!("组装 {}", memo),
        })?;
    }
    // 成品入库（数量 1）
    stock_insert(db, &StockMove {
        id: 0, period, biz_date: date, kind: StockKind::OtherIn,
        item: parent.to_string(), warehouse: String::new(), batch_no: String::new(),
        qty: Money::ONE, price: Money::ZERO, amount: Money::ZERO, voucher_id: None,
        memo: format!("组装 {}", memo),
    })?;
    Ok(())
}

/// 拆卸：1 个成品拆回多个子件（成品出库、子件入库）
pub fn disassemble(db: &Db, period: Period, date: NaiveDate, parent: &str, children: &[(String, Money)], memo: &str) -> DbResult<()> {
    use crate::business::{stock_insert, StockKind, StockMove};
    stock_insert(db, &StockMove {
        id: 0, period, biz_date: date, kind: StockKind::OtherOut,
        item: parent.to_string(), warehouse: String::new(), batch_no: String::new(),
        qty: Money::ONE.negated(), price: Money::ZERO, amount: Money::ZERO, voucher_id: None,
        memo: format!("拆卸 {}", memo),
    })?;
    for (item, qty) in children {
        stock_insert(db, &StockMove {
            id: 0, period, biz_date: date, kind: StockKind::OtherIn,
            item: item.clone(), warehouse: String::new(), batch_no: String::new(),
            qty: *qty, price: Money::ZERO, amount: Money::ZERO, voucher_id: None,
            memo: format!("拆卸 {}", memo),
        })?;
    }
    Ok(())
}

/// 形态转换（对标金蝶形态转换单）：源物料出库 → 目标物料入库，同数量一减一增。
/// **金额按源存货当前移动加权成本平移**（等值转换；目标 0 价首入会污染计价引擎与
/// 销售成本结转，故源无成本价时拒绝转换）；无总账凭证（存货内部结构调整）。
pub fn form_convert(
    db: &Db,
    period: Period,
    date: NaiveDate,
    from_item: &str,
    to_item: &str,
    qty: Money,
    memo: &str,
) -> DbResult<()> {
    if from_item == to_item {
        return Err(fincore::FinError::msg("源物料与目标物料不能相同").into());
    }
    if !from_item.trim().is_empty() && !to_item.trim().is_empty() && !qty.is_positive() {
        return Err(fincore::FinError::msg("转换数量必须大于 0").into());
    }
    let unit = crate::business::stock_state(
        db,
        from_item,
        period,
        fincore::engine::costing::CostMethod::MovingAverage,
    )?
    .unit_cost();
    if !unit.is_positive() {
        return Err(fincore::FinError::msg(
            "源物料无成本价，无法形态转换（请先入库带价或执行期末结价）",
        )
        .into());
    }
    let amount = qty * unit;
    use crate::business::{stock_insert, StockKind, StockMove};
    stock_insert(
        db,
        &StockMove {
            id: 0,
            period,
            biz_date: date,
            kind: StockKind::OtherOut,
            item: from_item.to_string(),
            warehouse: String::new(),
            batch_no: String::new(),
            qty: qty.negated(),
            price: unit,
            amount,
            voucher_id: None,
            memo: format!("形态转换 {}", memo),
        },
    )?;
    stock_insert(
        db,
        &StockMove {
            id: 0,
            period,
            biz_date: date,
            kind: StockKind::OtherIn,
            item: to_item.to_string(),
            warehouse: String::new(),
            batch_no: String::new(),
            qty,
            price: unit,
            amount,
            voucher_id: None,
            memo: format!("形态转换 {}", memo),
        },
    )?;
    Ok(())
}

/// 低于安全库存的存货：item_plan.safety_stock > 0 且现有库存（流水汇总）< 安全量。
/// 返回 (存货, 现有库存, 安全库存)——工作台仓管预警与低库存待办数据源。
pub fn below_safety(db: &Db) -> DbResult<Vec<(String, Money, Money)>> {
    let mut st = db.conn().prepare(
        "SELECT item_code, CAST(safety_stock AS REAL) FROM item_plan
         WHERE CAST(safety_stock AS REAL) > 0 ORDER BY item_code",
    )?;
    let plans: Vec<(String, f64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for (item, safety) in plans {
        let on: f64 = db.conn().query_row(
            "SELECT COALESCE(SUM(CAST(qty AS REAL)),0) FROM stock_move WHERE item=?1 AND qc_status=''",
            [&item],
            |r| r.get(0),
        )?;
        if on < safety {
            out.push((
                item,
                Money::parse_or_zero(&format!("{on:.4}")),
                Money::parse_or_zero(&format!("{safety:.4}")),
            ));
        }
    }
    Ok(out)
}

/// 库存状态：分仓库结存（含质检三口径：qty=结存、available=可用、pending=待检、quarantine=隔离）
#[derive(Clone, Debug, serde::Serialize)]
pub struct WhStock {
    pub warehouse: String,
    pub item: String,
    pub qty: Money,
    pub available: Money,
    pub pending: Money,
    pub quarantine: Money,
}

pub fn warehouse_stock(db: &Db, item: &str) -> DbResult<Vec<WhStock>> {
    let mut st = db.conn().prepare(
        "SELECT warehouse, qc_status, qty FROM stock_move WHERE item=?1 ORDER BY warehouse",
    )?;
    let rows = st
        .query_map([item], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                m(&r.get::<_, String>(2)?),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // 每仓四桶：结存 / 可用 / 待检 / 隔离（qc_status=''或未知→可用；pending/quarantine 各归其桶）
    let mut map: std::collections::BTreeMap<String, (Money, Money, Money, Money)> =
        std::collections::BTreeMap::new();
    for (w, qc, q) in rows {
        let e = map
            .entry(if w.is_empty() { "默认仓".to_string() } else { w })
            .or_insert((Money::ZERO, Money::ZERO, Money::ZERO, Money::ZERO));
        e.0 += q;
        match qc.as_str() {
            "pending" => e.2 += q,
            "quarantine" => e.3 += q,
            _ => e.1 += q,
        }
    }
    Ok(map
        .into_iter()
        .map(
            |(warehouse, (qty, available, pending, quarantine))| WhStock {
                warehouse,
                item: item.to_string(),
                qty,
                available,
                pending,
                quarantine,
            },
        )
        .collect())
}

/// 调拨报表：期间内调拨流水
pub fn transfer_report(db: &Db, period: Period) -> DbResult<Vec<crate::business::StockMove>> {
    let rows = crate::business::stock_list(db, period)?;
    Ok(rows.into_iter().filter(|r| r.kind == crate::business::StockKind::Transfer).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn serial_flow() {
        let db = mem();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        serial_in(&db, "P001", &["S1".into(), "S2".into(), "S3".into()], "B1", d).unwrap();
        assert_eq!(serial_list(&db, "P001").unwrap().len(), 3);
        serial_out(&db, &["S1".into()], d).unwrap();
        let list = serial_list(&db, "P001").unwrap();
        assert_eq!(list.iter().filter(|s| s.status == "in").count(), 2);
        assert_eq!(list.iter().filter(|s| s.status == "out").count(), 1);
    }

    #[test]
    fn unit_conversion() {
        let db = mem();
        unit_set(&db, &ItemUnit { item: "P001".into(), base_unit: "个".into(), alt_unit: "箱".into(), factor: m("10") }).unwrap();
        assert_eq!(unit_to_alt(&db, "P001", m("50")).unwrap(), m("5"));
        // 系数非法
        assert!(unit_set(&db, &ItemUnit { item: "P001".into(), base_unit: "个".into(), alt_unit: "箱".into(), factor: m("0") }).is_err());
    }

    #[test]
    fn abc_analysis_classes() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        use crate::business::{stock_insert, StockKind, StockMove};
        // A=700(70%)、B=200(90%)、C=100(100%) → A/B/C 三档分明
        for (item, qty, price) in [("A", "70", "10"), ("B", "20", "10"), ("C", "10", "10")] {
            stock_insert(&db, &StockMove {
                id: 0, period: p, biz_date: d, kind: StockKind::Purchase,
                item: item.into(), warehouse: "主仓".into(), batch_no: String::new(),
                qty: m(qty), price: m(price), amount: m(qty) * m(price), voucher_id: None, memo: String::new(),
            }).unwrap();
        }
        let rows = abc_analysis(&db, p).unwrap();
        assert_eq!(rows.len(), 3);
        // 按金额降序：A 700、B 200、C 100
        assert_eq!(rows[0].item, "A");
        assert_eq!(rows[0].class, "A");
        assert_eq!(rows[1].item, "B");
        assert_eq!(rows[1].class, "B");
        assert_eq!(rows[2].item, "C");
        assert_eq!(rows[2].class, "C");
    }

    #[test]
    fn assemble_disassemble() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        assemble(&db, p, d, "FG", &[("RM1".into(), m("2")), ("RM2".into(), m("1"))], "测试").unwrap();
        let fg = warehouse_stock(&db, "FG").unwrap();
        assert_eq!(fg.iter().map(|w| w.qty).sum::<Money>(), m("1"));
        let rm1 = warehouse_stock(&db, "RM1").unwrap();
        assert_eq!(rm1.iter().map(|w| w.qty).sum::<Money>(), m("-2"));
        disassemble(&db, p, d, "FG", &[("RM1".into(), m("2"))], "拆").unwrap();
        let fg = warehouse_stock(&db, "FG").unwrap();
        assert_eq!(fg.iter().map(|w| w.qty).sum::<Money>(), m("0"));
    }
}
