//! 制造业成本核算
//!
//! 支持生产订单的成本归集、分配与完工结转。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult, FinError};
use crate::scm::{BomItem, ProdStatus, ProductionOrder};

// ===========================================================================
// 成本归集
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum CostType {
    Material,
    Labor,
    Overhead,
}

impl CostType {
    pub fn label(self) -> &'static str {
        match self {
            CostType::Material => "直接材料",
            CostType::Labor => "直接人工",
            CostType::Overhead => "制造费用",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            CostType::Material => "material",
            CostType::Labor => "labor",
            CostType::Overhead => "overhead",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProdCostItem {
    pub id: i64,
    pub po_id: i64,
    pub cost_type: CostType,
    pub amount: Money,
    pub memo: String,
}

// ===========================================================================
// 数据库操作
// ===========================================================================

pub fn add_cost(db: &Db, po_id: i64, cost_type: CostType, amount: Money, memo: &str) -> DbResult<i64> {
    let tx = db.conn().unchecked_transaction()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    
    tx.execute(
        "INSERT INTO prod_cost(po_id, cost_type, amount, memo, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            po_id,
            cost_type.code(),
            amount.to_string(),
            memo,
            now
        ],
    )?;
    
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(id)
}

pub fn get_prod_cost(db: &Db, po_id: i64) -> DbResult<(Money, Money, Money)> {
    // 使用TEXT聚合避免浮点精度问题
    let mat_sql = "SELECT COALESCE(GROUP_CONCAT(amount, '+'), '0') FROM prod_cost WHERE po_id=? AND cost_type='material'";
    let lab_sql = "SELECT COALESCE(GROUP_CONCAT(amount, '+'), '0') FROM prod_cost WHERE po_id=? AND cost_type='labor'";
    let oh_sql = "SELECT COALESCE(GROUP_CONCAT(amount, '+'), '0') FROM prod_cost WHERE po_id=? AND cost_type='overhead'";
    
    let mat_str: String = db.conn().query_row(mat_sql, [po_id], |r| r.get(0))?;
    let lab_str: String = db.conn().query_row(lab_sql, [po_id], |r| r.get(0))?;
    let oh_str: String = db.conn().query_row(oh_sql, [po_id], |r| r.get(0))?;
    
    // 解析累加公式
    let parse_sum = |s: &str| -> Money {
        if s == "0" { return Money::ZERO; }
        s.split('+').map(|x| Money::parse_or_zero(x)).sum()
    };
    
    Ok((
        parse_sum(&mat_str),
        parse_sum(&lab_str),
        parse_sum(&oh_str),
    ))
}

pub fn get_prod_total_cost(db: &Db, po_id: i64) -> DbResult<Money> {
    let (mat, lab, oh) = get_prod_cost(db, po_id)?;
    Ok(mat + lab + oh)
}

// ===========================================================================
// BOM展开与领料
// ===========================================================================

pub fn prod_issue_materials(
    db: &Db,
    po_id: i64,
    issue_date: NaiveDate,
    period: Period,
    who: &str,
) -> DbResult<Vec<(String, Money, Money)>> {
    use crate::scm::bom_list;
    use crate::business::{stock_insert, StockMove, StockKind};
    
    let order = match get_prod_order(db, po_id)? {
        Some(o) => o,
        None => return Err(FinError::msg("生产订单不存在").into()),
    };
    
    let bom_items = bom_list(db, &order.item_code)?;
    if bom_items.is_empty() {
        return Err(FinError::msg(format!("产品 {} 无BOM，无法领料", order.item_code)).into());
    }
    
    let mut results = Vec::new();
    
    for item in &bom_items {
        let qty_required = item.qty * order.planned_qty * (Money::ONE + item.loss_rate);
        
        let mut move_record = StockMove {
            id: 0,
            period,
            biz_date: issue_date,
            kind: StockKind::OtherOut,
            item: item.child_code.clone(),
            warehouse: String::new(),
            batch_no: String::new(),
            qty: -qty_required,
            price: Money::ZERO,
            amount: Money::ZERO,
            voucher_id: None,
            memo: format!("生产领料 PO#{}", order.no),
        };
        
        stock_insert(db, &mut move_record)?;
        
        let unit_cost = get_item_cost(db, &item.child_code, period.ymm())?;
        let amount = qty_required.abs() * unit_cost;
        
        results.push((item.child_code.clone(), qty_required.abs(), amount));
    }
    
    Ok(results)
}

fn get_item_cost(db: &Db, item_code: &str, period_ymm: i32) -> DbResult<Money> {
    let sql = "SELECT price FROM stock_move
               WHERE item=? AND kind='purchase' AND period<=?
               ORDER BY biz_date DESC LIMIT 1";

    db.conn()
        .query_row(sql, [item_code, &period_ymm.to_string()], |r| r.get::<_, String>(0))
        .optional()?
        .map(|s| Money::parse_or_zero(&s))
        .ok_or_else(|| FinError::msg("物料成本未知").into())
}

// ===========================================================================
// 完工入库
// ===========================================================================

pub fn prod_complete(
    db: &Db,
    po_id: i64,
    complete_date: NaiveDate,
    period: Period,
    completed_qty: Money,
    who: &str,
) -> DbResult<i64> {
    use crate::business::{stock_insert, StockMove, StockKind};
    
    let order = match get_prod_order(db, po_id)? {
        Some(o) => o,
        None => return Err(FinError::msg("生产订单不存在").into()),
    };
    
    if order.status != ProdStatus::InProgress {
        return Err(FinError::msg("只有进行中的订单才能完工入库").into());
    }
    
    let total_cost = get_prod_total_cost(db, po_id)?;
    let unit_cost = if completed_qty > Money::ZERO {
        total_cost / completed_qty
    } else {
        Money::ZERO
    };
    
    let mut move_record = StockMove {
        id: 0,
        period,
        biz_date: complete_date,
        kind: StockKind::OtherIn,
        item: order.item_code.clone(),
        warehouse: String::new(),
        batch_no: String::new(),
        qty: completed_qty,
        price: unit_cost,
        amount: total_cost,
        voucher_id: None,
        memo: format!("完工入库 PO#{}", order.no),
    };
    
    let move_id = stock_insert(db, &mut move_record)?;
    
    let tx = db.conn().unchecked_transaction()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    tx.execute(
        "UPDATE production_order SET status='completed', completed_qty=?, updated_at=? WHERE id=?",
        rusqlite::params![completed_qty.to_string(), now, po_id]
    )?;
    tx.commit()?;
    
    Ok(move_id)
}

pub fn get_prod_order(db: &Db, po_id: i64) -> DbResult<Option<ProductionOrder>> {
    let row = db.conn()
        .query_row(
            "SELECT id, no, period, date, item_code, item_name, planned_qty, completed_qty, status, work_center, prepared_by, memo
             FROM production_order WHERE id=?",
            [po_id],
            |r| Ok(ProductionOrder {
                id: r.get(0)?,
                no: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: r.get(3)?,
                item_code: r.get(4)?,
                item_name: r.get(5)?,
                planned_qty: Money::parse_or_zero(&r.get::<_, String>(6)?),
                completed_qty: Money::parse_or_zero(&r.get::<_, String>(7)?),
                status: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or(ProdStatus::Draft),
                work_center: r.get(9)?,
                prepared_by: r.get(10)?,
                memo: r.get(11)?,
            }),
        );
    match row {
        Ok(order) => Ok(Some(order)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    
    #[test]
    fn cost_tracking() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        
        let tx = db.conn().unchecked_transaction().unwrap();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        tx.execute(
            "INSERT INTO production_order(period, no, date, item_code, item_name, planned_qty, completed_qty, status, work_center, prepared_by, memo, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                p.ymm(), "SC2026010001", "2026-01-05", "1001", "成品A",
                "100", "0", "in_progress", "WC01", "u1", "",
                now.clone(), now.clone()
            ],
        ).unwrap();
        let po_id = tx.last_insert_rowid();
        tx.commit().unwrap();
        
        add_cost(&db, po_id, CostType::Material, Money::parse("5000").unwrap(), "领料").unwrap();
        add_cost(&db, po_id, CostType::Labor, Money::parse("1000").unwrap(), "人工").unwrap();
        add_cost(&db, po_id, CostType::Overhead, Money::parse("500").unwrap(), "制造费用").unwrap();
        
        let (mat, lab, oh) = get_prod_cost(&db, po_id).unwrap();
        assert_eq!(mat, Money::parse("5000").unwrap());
        assert_eq!(lab, Money::parse("1000").unwrap());
        assert_eq!(oh, Money::parse("500").unwrap());
        
        let total = get_prod_total_cost(&db, po_id).unwrap();
        assert_eq!(total, Money::parse("6500").unwrap());
    }
}
