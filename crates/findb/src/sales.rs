//! 销售深化：报价单 / 发货 / 收款 / 退货 / 统计 / 执行跟踪 / 信用管理
//!
//! 对标金蝶/用友销售管理。金额一律 TEXT 存储、Rust 侧 Decimal 累加。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

// ===========================================================================
// 销售报价单
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Quotation {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    pub customer_code: String,
    pub customer_name: String,
    pub item_code: String,
    pub item_name: String,
    pub qty: Money,
    pub unit_price: Money,
    pub status: String, // draft / approved / converted / cancelled
    pub prepared_by: String,
    pub memo: String,
}

fn map_quo(r: &rusqlite::Row) -> rusqlite::Result<Quotation> {
    Ok(Quotation {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&r.get::<_, String>(3)?, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        customer_code: r.get(4)?,
        customer_name: r.get(5)?,
        item_code: r.get(6)?,
        item_name: r.get(7)?,
        qty: m(&r.get::<_, String>(8)?),
        unit_price: m(&r.get::<_, String>(9)?),
        status: r.get(10)?,
        prepared_by: r.get(11)?,
        memo: r.get(12)?,
    })
}

const Q_COLS: &str = "id,no,period,date,customer_code,customer_name,item_code,item_name,qty,unit_price,status,prepared_by,memo";

pub fn quo_next_no(db: &Db, period: Period) -> DbResult<String> {
    let prefix = format!("BJ{:04}{:02}", period.year(), period.month());
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM quotation WHERE no LIKE ?1",
        rusqlite::params![format!("{prefix}%")],
        |r| r.get(0),
    )?;
    Ok(format!("{prefix}-{:03}", n + 1))
}

pub fn quo_save(db: &Db, q: &mut Quotation) -> DbResult<i64> {
    let id = if q.id > 0 {
        db.conn().execute(
            "UPDATE quotation SET period=?2,date=?3,customer_code=?4,customer_name=?5,
             item_code=?6,item_name=?7,qty=?8,unit_price=?9,status=?10,prepared_by=?11,memo=?12
             WHERE id=?1",
            rusqlite::params![
                q.id, q.period.ymm(), q.date.format("%Y-%m-%d").to_string(),
                q.customer_code, q.customer_name, q.item_code, q.item_name,
                crate::exact_param(q.qty), crate::exact_param(q.unit_price), q.status, q.prepared_by, q.memo
            ],
        )?;
        q.id
    } else {
        db.conn().execute(
            "INSERT INTO quotation(no,period,date,customer_code,customer_name,item_code,item_name,qty,unit_price,status,prepared_by,memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![
                q.no, q.period.ymm(), q.date.format("%Y-%m-%d").to_string(),
                q.customer_code, q.customer_name, q.item_code, q.item_name,
                crate::exact_param(q.qty), crate::exact_param(q.unit_price), q.status, q.prepared_by, q.memo
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    q.id = id;
    Ok(id)
}

pub fn quo_list(db: &Db, period: Period) -> DbResult<Vec<Quotation>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {Q_COLS} FROM quotation WHERE period=?1 ORDER BY id DESC"
    ))?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], map_quo)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn quo_get(db: &Db, id: i64) -> DbResult<Option<Quotation>> {
    db.conn()
        .query_row(&format!("SELECT {Q_COLS} FROM quotation WHERE id=?1"), [id], map_quo)
        .optional()
        .map_err(Into::into)
}

/// 报价单审批：draft → approved
pub fn quo_approve(db: &Db, id: i64) -> DbResult<()> {
    let q = quo_get(db, id)?.ok_or_else(|| fincore::FinError::msg("报价单不存在"))?;
    if q.status != "draft" {
        return Err(fincore::FinError::msg(format!("报价单已{}，不能审批", q.status)).into());
    }
    db.conn().execute(
        "UPDATE quotation SET status='approved' WHERE id=?1",
        [id],
    )?;
    Ok(())
}

// ===========================================================================
// 发货 / 收款 / 退货
// ===========================================================================

pub fn so_shipment_add(db: &Db, so_id: i64, period: Period, date: NaiveDate, qty: Money, memo: &str) -> DbResult<i64> {
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("发货数量必须为正数").into());
    }
    db.conn().execute(
        "INSERT INTO so_shipment(so_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![so_id, period.ymm(), date.format("%Y-%m-%d").to_string(), crate::exact_param(qty), memo],
    )?;
    Ok(db.conn().last_insert_rowid())
}

/// 销售退货：负发货记录
pub fn so_return_add(db: &Db, so_id: i64, period: Period, date: NaiveDate, qty: Money, memo: &str) -> DbResult<i64> {
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("退货数量必须为正数").into());
    }
    db.conn().execute(
        "INSERT INTO so_shipment(so_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![so_id, period.ymm(), date.format("%Y-%m-%d").to_string(), qty.negated().to_string(), format!("退货 {}", memo)],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn so_shipment_sum(db: &Db, so_id: i64) -> DbResult<Money> {
    let mut st = db.conn().prepare("SELECT qty FROM so_shipment WHERE so_id=?1")?;
    let rows = st
        .query_map([so_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| m(s)).sum())
}

pub fn so_payment_add(db: &Db, so_id: i64, period: Period, date: NaiveDate, amount: Money, memo: &str) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO so_payment(so_id,period,date,amount,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![so_id, period.ymm(), date.format("%Y-%m-%d").to_string(), crate::money_param(amount), memo],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn so_payment_sum(db: &Db, so_id: i64) -> DbResult<Money> {
    let mut st = db.conn().prepare("SELECT amount FROM so_payment WHERE so_id=?1")?;
    let rows = st
        .query_map([so_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| m(s)).sum())
}

// ===========================================================================
// 统计 / 执行跟踪 / 信用管理
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize)]
pub struct SalesStat {
    pub customer_code: String,
    pub customer_name: String,
    pub order_count: i64,
    pub amount: Money,
}

pub fn sales_stats(db: &Db, period: Period) -> DbResult<Vec<SalesStat>> {
    let orders = crate::scm::so_list(db, period, None)?;
    let mut map: std::collections::BTreeMap<String, SalesStat> = std::collections::BTreeMap::new();
    for o in orders {
        let e = map.entry(o.customer_code.clone()).or_insert_with(|| SalesStat {
            customer_code: o.customer_code.clone(),
            customer_name: o.customer_name.clone(),
            order_count: 0,
            amount: Money::ZERO,
        });
        e.order_count += 1;
        e.amount += o.total_amount;
    }
    Ok(map.into_values().collect())
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SoTrack {
    pub so_id: i64,
    pub no: String,
    pub customer_name: String,
    pub ordered_qty: Money,
    pub shipped_qty: Money,
    pub rate: Money,
}

pub fn so_execution_track(db: &Db, period: Period) -> DbResult<Vec<SoTrack>> {
    let orders = crate::scm::so_list(db, period, None)?;
    let mut out = Vec::new();
    for o in orders {
        if matches!(o.status, crate::scm::SoStatus::Cancelled) {
            continue;
        }
        let ordered: Money = o.lines.iter().map(|l| l.qty_ordered).sum();
        let shipped = so_shipment_sum(db, o.id)?;
        let rate = if ordered.is_zero() {
            Money::ZERO
        } else {
            ((shipped.abs() * Money::from_i64(100)) / ordered.abs().inner()).round2()
        };
        out.push(SoTrack {
            so_id: o.id,
            no: o.no,
            customer_name: o.customer_name,
            ordered_qty: ordered,
            shipped_qty: shipped,
            rate,
        });
    }
    Ok(out)
}

/// 客户信用额度（辅助档案 props.credit_limit），0 = 未设额度
pub fn customer_credit_limit(db: &Db, customer_code: &str) -> DbResult<Money> {
    let props: Option<String> = db.conn().query_row(
        "SELECT props_json FROM aux_entity WHERE kind='customer' AND code=?1",
        [customer_code],
        |r| r.get(0),
    ).optional()?;
    let Some(props) = props else { return Ok(Money::ZERO) };
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&props).unwrap_or_default();
    Ok(map.get("credit_limit").map(|s| m(s)).unwrap_or(Money::ZERO))
}

/// 信用检查：某客户累计应收（销售订单总额 − 已收款）是否超额度。
/// 返回 (累计占用, 信用额度, 是否超限)。
pub fn credit_check(db: &Db, customer_code: &str, period: Period) -> DbResult<(Money, Money, bool)> {
    let limit = customer_credit_limit(db, customer_code)?;
    let orders = crate::scm::so_list(db, period, None)?;
    let mut receivable = Money::ZERO;
    for o in orders.iter().filter(|o| o.customer_code == customer_code && !matches!(o.status, crate::scm::SoStatus::Cancelled)) {
        receivable += o.total_amount;
        receivable -= so_payment_sum(db, o.id)?;
    }
    let over = !limit.is_zero() && receivable > limit;
    Ok((receivable, limit, over))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn quotation_flow() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut q = Quotation {
            id: 0, no: quo_next_no(&db, p).unwrap(), period: p,
            date: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            customer_code: "C01".into(), customer_name: "客户A".into(),
            item_code: "140501".into(), item_name: "成品".into(),
            qty: m("50"), unit_price: m("20"), status: "draft".into(),
            prepared_by: "张三".into(), memo: String::new(),
        };
        let id = quo_save(&db, &mut q).unwrap();
        quo_approve(&db, id).unwrap();
        assert_eq!(quo_get(&db, id).unwrap().unwrap().status, "approved");
        assert_eq!(quo_list(&db, p).unwrap().len(), 1);
    }

    #[test]
    fn shipment_payment_track() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut so = crate::scm::SalesOrder::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "C01", "客户A", "u");
        so.no = crate::scm::so_next_no(&db, p).unwrap();
        so.lines.push(crate::scm::SoLine {
            id: 0, so_id: 0, item_code: "140501".into(), item_name: "成品".into(),
            qty_ordered: m("100"), qty_shipped: m("0"), unit_price: m("20"),
            tax_rate: m("0"), amount: m("2000"), tax_amount: m("0"), memo: String::new(),
        });
        let so_id = crate::scm::so_save(&db, &mut so).unwrap();
        so_shipment_add(&db, so_id, p, NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(), m("70"), "").unwrap();
        so_return_add(&db, so_id, p, NaiveDate::from_ymd_opt(2026, 1, 12).unwrap(), m("10"), "拒收").unwrap();
        so_payment_add(&db, so_id, p, NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(), m("1200"), "").unwrap();
        assert_eq!(so_shipment_sum(&db, so_id).unwrap(), m("60"));
        assert_eq!(so_payment_sum(&db, so_id).unwrap(), m("1200"));
        let track = so_execution_track(&db, p).unwrap();
        assert_eq!(track[0].shipped_qty, m("60"));
        assert_eq!(track[0].rate, m("60"));
        let stats = sales_stats(&db, p).unwrap();
        assert_eq!(stats[0].amount, m("2000"));
    }

    #[test]
    fn credit_check_basic() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 未设额度 → 不超限
        let (recv, limit, over) = credit_check(&db, "C01", p).unwrap();
        assert_eq!(limit, m("0"));
        assert_eq!(recv, m("0"));
        assert!(!over);
    }
}
