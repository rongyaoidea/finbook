//! 采购/销售深度：暂估、对账、配额、订单变更
//!
//! 对标金蝶/用友供应链。金额一律 TEXT 存储、Rust 侧 Decimal 累加。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}
fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// ===========================================================================
// 采购暂估
// ===========================================================================

/// 暂估：入库未到票，先按估计金额挂账
pub fn po_estimate_add(db: &Db, po_id: i64, period: Period, item: &str, est_amount: Money) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO po_estimate(po_id,period,item,est_amount,settled) VALUES(?1,?2,?3,?4,0)",
        rusqlite::params![po_id, period.ymm(), item, crate::money_param(est_amount)],
    )?;
    Ok(db.conn().last_insert_rowid())
}

/// 暂估冲回：发票到票后标记 settled
pub fn po_estimate_settle(db: &Db, id: i64) -> DbResult<()> {
    db.conn().execute("UPDATE po_estimate SET settled=1 WHERE id=?1", [id])?;
    Ok(())
}

/// 未冲回的暂估合计
pub fn po_estimate_open_sum(db: &Db, po_id: i64) -> DbResult<Money> {
    let mut st = db.conn().prepare("SELECT est_amount FROM po_estimate WHERE po_id=?1 AND settled=0")?;
    let rows = st
        .query_map([po_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| m(s)).sum())
}

/// 某采购订单的暂估明细
#[derive(Clone, Debug, serde::Serialize)]
pub struct PoEstimate {
    pub id: i64,
    pub po_id: i64,
    pub period: i32,
    pub item: String,
    pub est_amount: Money,
    pub settled: bool,
}

/// 列出某采购订单的全部暂估明细
pub fn po_estimate_list(db: &Db, po_id: i64) -> DbResult<Vec<PoEstimate>> {
    let mut st = db.conn().prepare(
        "SELECT id, po_id, period, item, est_amount, settled FROM po_estimate WHERE po_id=?1 ORDER BY id",
    )?;
    let rows = st
        .query_map([po_id], |r| {
            Ok(PoEstimate {
                id: r.get(0)?,
                po_id: r.get(1)?,
                period: r.get(2)?,
                item: r.get(3)?,
                est_amount: m(&r.get::<_, String>(4)?),
                settled: r.get::<_, i64>(5)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ===========================================================================
// 对账
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize)]
pub struct PoRecon {
    pub po_id: i64,
    pub no: String,
    pub supplier: String,
    /// 订单金额
    pub order_amount: Money,
    /// 已付款
    pub paid: Money,
    /// 未付款（应付）
    pub unpaid: Money,
    /// 未冲回暂估
    pub open_estimate: Money,
}

/// 采购对账：订单金额 vs 付款 vs 暂估
pub fn po_reconcile(db: &Db, period: Period) -> DbResult<Vec<PoRecon>> {
    let orders = crate::scm::po_list(db, period, None)?;
    let mut out = Vec::new();
    for o in orders {
        if matches!(o.status, crate::scm::PoStatus::Cancelled) {
            continue;
        }
        let paid = crate::procurement::po_payment_sum(db, o.id)?;
        let est = po_estimate_open_sum(db, o.id)?;
        out.push(PoRecon {
            po_id: o.id,
            no: o.no,
            supplier: o.supplier_name,
            order_amount: o.total_amount,
            paid,
            unpaid: o.total_amount - paid,
            open_estimate: est,
        });
    }
    Ok(out)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SoRecon {
    pub so_id: i64,
    pub no: String,
    pub customer: String,
    pub order_amount: Money,
    pub received: Money,
    /// 未收（应收）
    pub unreceived: Money,
}

/// 销售对账：订单金额 vs 收款
pub fn so_reconcile(db: &Db, period: Period) -> DbResult<Vec<SoRecon>> {
    let orders = crate::scm::so_list(db, period, None)?;
    let mut out = Vec::new();
    for o in orders {
        if matches!(o.status, crate::scm::SoStatus::Cancelled) {
            continue;
        }
        let received = crate::sales::so_payment_sum(db, o.id)?;
        out.push(SoRecon {
            so_id: o.id,
            no: o.no,
            customer: o.customer_name,
            order_amount: o.total_amount,
            received,
            unreceived: o.total_amount - received,
        });
    }
    Ok(out)
}

// ===========================================================================
// 配额
// ===========================================================================

/// 供应商配额：设置某期间某供应商某物料的可采购上限
pub fn quota_set(db: &Db, period: Period, supplier: &str, item: &str, quota_qty: Money) -> DbResult<()> {
    if quota_qty < Money::ZERO {
        return Err(fincore::FinError::msg("配额不能为负").into());
    }
    db.conn().execute(
        "INSERT INTO supplier_quota(period,supplier_code,item,quota_qty,used_qty) VALUES(?1,?2,?3,?4,'0')
         ON CONFLICT(period,supplier_code,item) DO UPDATE SET quota_qty=excluded.quota_qty",
        rusqlite::params![period.ymm(), supplier, item, crate::exact_param(quota_qty)],
    )?;
    Ok(())
}

/// 配额占用：采购订单保存后累计已用数量（读改写进同一事务，避免丢更新）
pub fn quota_use(db: &Db, period: Period, supplier: &str, item: &str, qty: Money) -> DbResult<()> {
    let tx = db.write_tx()?;
    let used: Option<String> = tx
        .query_row(
            "SELECT used_qty FROM supplier_quota WHERE period=?1 AND supplier_code=?2 AND item=?3",
            rusqlite::params![period.ymm(), supplier, item],
            |r| r.get(0),
        )
        .optional()?;
    let new_used = used.map(|s| m(&s)).unwrap_or(Money::ZERO) + qty;
    tx.execute(
        "UPDATE supplier_quota SET used_qty=?2 WHERE period=?1 AND supplier_code=?3 AND item=?4",
        rusqlite::params![period.ymm(), crate::exact_param(new_used), supplier, item],
    )?;
    tx.commit()?;
    Ok(())
}

/// 配额检查：某供应商某物料的剩余配额（quota 未设置返回 None）
pub fn quota_remaining(db: &Db, period: Period, supplier: &str, item: &str) -> DbResult<Option<Money>> {
    let row: Option<(String, String)> = db.conn().query_row(
        "SELECT quota_qty, used_qty FROM supplier_quota WHERE period=?1 AND supplier_code=?2 AND item=?3",
        rusqlite::params![period.ymm(), supplier, item],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    match row {
        Some((q, u)) => Ok(Some(m(&q) - m(&u))),
        None => Ok(None),
    }
}

// ===========================================================================
// 订单变更历史
// ===========================================================================

pub fn change_log_add(db: &Db, order_type: &str, order_id: i64, field: &str, old_value: &str, new_value: &str, who: &str) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO order_change_log(order_type,order_id,field,old_value,new_value,changed_by,changed_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![order_type, order_id, field, old_value, new_value, who, now()],
    )?;
    Ok(())
}

pub fn change_log_list(db: &Db, order_type: &str, order_id: i64) -> DbResult<Vec<(String, String, String, String, String)>> {
    let mut st = db.conn().prepare(
        "SELECT field, old_value, new_value, changed_by, changed_at FROM order_change_log
         WHERE order_type=?1 AND order_id=?2 ORDER BY id DESC",
    )?;
    let rows = st
        .query_map(rusqlite::params![order_type, order_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn estimate_and_reconcile() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = crate::scm::PurchaseOrder::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "S01", "供应商A", "u");
        po.no = crate::scm::po_next_no(&db, p).unwrap();
        po.lines.push(crate::scm::PoLine {
            id: 0, po_id: 0, item_code: "140301".into(), item_name: "原料".into(),
            qty_ordered: m("100"), qty_received: m("0"), unit_price: m("10"),
            tax_rate: m("0"), amount: m("1000"), tax_amount: m("0"), memo: String::new(),
        });
        let po_id = crate::scm::po_save(&db, &mut po).unwrap();
        // 暂估 800
        let est_id = po_estimate_add(&db, po_id, p, "140301", m("800")).unwrap();
        assert_eq!(po_estimate_open_sum(&db, po_id).unwrap(), m("800"));
        po_estimate_settle(&db, est_id).unwrap();
        assert_eq!(po_estimate_open_sum(&db, po_id).unwrap(), m("0"));
        // 付款 600 → 对账未付 400
        crate::procurement::po_payment_add(&db, &crate::procurement::PoPayment {
            id: 0, po_id, period: p, date: NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
            amount: m("600"), memo: String::new(),
        }).unwrap();
        let r = po_reconcile(&db, p).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].order_amount, m("1000"));
        assert_eq!(r[0].unpaid, m("400"));
    }

    #[test]
    fn quota_tracking() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        quota_set(&db, p, "S01", "140301", m("500")).unwrap();
        assert_eq!(quota_remaining(&db, p, "S01", "140301").unwrap(), Some(m("500")));
        quota_use(&db, p, "S01", "140301", m("120")).unwrap();
        assert_eq!(quota_remaining(&db, p, "S01", "140301").unwrap(), Some(m("380")));
        // 未设置配额的返回 None
        assert_eq!(quota_remaining(&db, p, "S01", "9999").unwrap(), None);
    }

    #[test]
    fn change_log_recorded() {
        let db = mem();
        change_log_add(&db, "po", 1, "status", "draft", "confirmed", "张三").unwrap();
        let log = change_log_list(&db, "po", 1).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "status");
        assert_eq!(log[0].2, "confirmed");
    }
}
