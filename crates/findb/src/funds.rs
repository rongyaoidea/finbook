//! 资金管理：票据、融资、资金日报、资金预测
//!
//! 对标金蝶/用友资金模块。金额一律 TEXT 存储、Rust 侧 Decimal 累加。
//! - 票据：应收/应付票据台账，状态流转（在库 → 背书/贴现/到期/兑付）
//! - 融资：借款/放款台账（本金、年利率、起止日期）
//! - 资金日报：各现金/银行科目的期初、收入、支出、结存
//! - 资金预测：现金结存 + 应收票据 − 应付票据 + 放款 − 借款，给出资金头寸

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}
fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

// ===========================================================================
// 票据
// ===========================================================================

/// 票据状态
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BillStatus {
    /// 在库（持有，未流转）
    InHand,
    /// 已背书（转让给他人）
    Endorsed,
    /// 已贴现
    Discounted,
    /// 已到期（未兑付）
    Matured,
    /// 已兑付 / 已付清
    Settled,
}

impl BillStatus {
    pub fn label(self) -> &'static str {
        match self {
            BillStatus::InHand => "在库",
            BillStatus::Endorsed => "已背书",
            BillStatus::Discounted => "已贴现",
            BillStatus::Matured => "已到期",
            BillStatus::Settled => "已兑付",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            BillStatus::InHand => "in_hand",
            BillStatus::Endorsed => "endorsed",
            BillStatus::Discounted => "discounted",
            BillStatus::Matured => "matured",
            BillStatus::Settled => "settled",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "endorsed" => BillStatus::Endorsed,
            "discounted" => BillStatus::Discounted,
            "matured" => BillStatus::Matured,
            "settled" => BillStatus::Settled,
            _ => BillStatus::InHand,
        }
    }
    pub const ALL: &'static [BillStatus] = &[
        BillStatus::InHand,
        BillStatus::Endorsed,
        BillStatus::Discounted,
        BillStatus::Matured,
        BillStatus::Settled,
    ];
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Bill {
    pub id: i64,
    pub kind: String, // receivable / payable
    pub no: String,
    pub period: Period,
    pub issue_date: NaiveDate,
    pub due_date: NaiveDate,
    pub counterpart: String,
    pub bank: String,
    pub amount: Money,
    pub status: String,
    pub handled_date: Option<NaiveDate>,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
}

impl Bill {
    /// 应收票据为正、应付票据为负（用于资金头寸）
    pub fn signed(&self) -> Money {
        if self.kind == "receivable" {
            self.amount
        } else {
            -self.amount
        }
    }
}

fn map_bill(r: &rusqlite::Row) -> rusqlite::Result<Bill> {
    let issue: String = r.get(3)?;
    let due: String = r.get(4)?;
    let handled: Option<String> = r.get(10)?;
    Ok(Bill {
        id: r.get(0)?,
        kind: r.get(1)?,
        no: r.get(2)?,
        period: Period::from_ymm(r.get(6)?),
        issue_date: NaiveDate::parse_from_str(&issue, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        due_date: NaiveDate::parse_from_str(&due, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        counterpart: r.get(7)?,
        bank: r.get(8)?,
        amount: m(&r.get::<_, String>(9)?),
        status: r.get(5)?,
        handled_date: handled.map(|s| {
            NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
        }),
        memo: r.get(11)?,
        created_by: r.get(12)?,
        created_at: r.get(13)?,
    })
}

const B_COLS: &str = "id,kind,no,issue_date,due_date,status,period,counterpart,bank,amount,\
     handled_date,memo,created_by,created_at";

pub fn bill_list(db: &Db, kind: Option<&str>) -> DbResult<Vec<Bill>> {
    let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match kind {
        Some(k) => (
            format!("SELECT {B_COLS} FROM bill WHERE kind=?1 ORDER BY due_date, id DESC"),
            vec![Box::new(k.to_string())],
        ),
        None => (
            format!("SELECT {B_COLS} FROM bill ORDER BY due_date, id DESC"),
            vec![],
        ),
    };
    let mut st = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = st
        .query_map(refs.as_slice(), map_bill)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn bill_get(db: &Db, id: i64) -> DbResult<Option<Bill>> {
    db.conn()
        .query_row(
            &format!("SELECT {B_COLS} FROM bill WHERE id=?1"),
            rusqlite::params![id],
            map_bill,
        )
        .optional()
        .map_err(Into::into)
}

pub fn bill_save(db: &Db, b: &mut Bill) -> DbResult<i64> {
    let id = if b.id > 0 {
        db.conn().execute(
            "UPDATE bill SET kind=?2, no=?3, issue_date=?4, due_date=?5, status=?6, period=?7,
             counterpart=?8, bank=?9, amount=?10, handled_date=?11, memo=?12 WHERE id=?1",
            rusqlite::params![
                b.id, b.kind, b.no,
                b.issue_date.format("%Y-%m-%d").to_string(),
                b.due_date.format("%Y-%m-%d").to_string(),
                b.status, b.period.ymm(), b.counterpart, b.bank, b.amount.to_string(),
                b.handled_date.map(|d| d.format("%Y-%m-%d").to_string()), b.memo
            ],
        )?;
        b.id
    } else {
        db.conn().execute(
            "INSERT INTO bill(kind,no,issue_date,due_date,status,period,counterpart,bank,amount,
             handled_date,memo,created_by,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                b.kind, b.no,
                b.issue_date.format("%Y-%m-%d").to_string(),
                b.due_date.format("%Y-%m-%d").to_string(),
                b.status, b.period.ymm(), b.counterpart, b.bank, b.amount.to_string(),
                b.handled_date.map(|d| d.format("%Y-%m-%d").to_string()), b.memo,
                b.created_by, now()
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    b.id = id;
    Ok(id)
}

pub fn bill_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM bill WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 票据状态流转：背书 / 贴现 / 到期 / 兑付，落 handled_date
pub fn bill_transition(
    db: &Db,
    id: i64,
    to: BillStatus,
    date: NaiveDate,
) -> DbResult<()> {
    let mut b = bill_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("票据"))?;
    if b.status == BillStatus::Settled.code() || b.status == BillStatus::Endorsed.code() {
        // 已背书/已兑付的票据为终态，不允许再流转（贴现需从在库发起）
        if to != BillStatus::Settled {
            return Err(fincore::FinError::state("该票据已背书或已兑付，不能继续流转").into());
        }
    }
    b.status = to.code().to_string();
    b.handled_date = Some(date);
    bill_save(db, &mut b)?;
    Ok(())
}

// ===========================================================================
// 融资（借款 / 放款）
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Loan {
    pub id: i64,
    pub kind: String, // borrow / lend
    pub no: String,
    pub bank: String,
    pub principal: Money,
    /// 年利率（%）
    pub rate_pct: Money,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub status: String, // active / settled
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
}

impl Loan {
    /// 借款为负（未来要还）、放款为正（未来收回）
    pub fn signed(&self) -> Money {
        if self.kind == "lend" {
            self.principal
        } else {
            -self.principal
        }
    }
}

fn map_loan(r: &rusqlite::Row) -> rusqlite::Result<Loan> {
    let s: String = r.get(5)?;
    let e: String = r.get(6)?;
    Ok(Loan {
        id: r.get(0)?,
        kind: r.get(1)?,
        no: r.get(2)?,
        bank: r.get(3)?,
        principal: m(&r.get::<_, String>(4)?),
        rate_pct: m(&r.get::<_, String>(8)?),
        start_date: NaiveDate::parse_from_str(&s, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        end_date: NaiveDate::parse_from_str(&e, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        status: r.get(7)?,
        memo: r.get(9)?,
        created_by: r.get(10)?,
        created_at: r.get(11)?,
    })
}

const L_COLS: &str = "id,kind,no,bank,principal,start_date,end_date,status,rate_pct,memo,created_by,created_at";

pub fn loan_list(db: &Db, kind: Option<&str>) -> DbResult<Vec<Loan>> {
    let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match kind {
        Some(k) => (
            format!("SELECT {L_COLS} FROM loan WHERE kind=?1 ORDER BY end_date, id DESC"),
            vec![Box::new(k.to_string())],
        ),
        None => (
            format!("SELECT {L_COLS} FROM loan ORDER BY end_date, id DESC"),
            vec![],
        ),
    };
    let mut st = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = st
        .query_map(refs.as_slice(), map_loan)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn loan_get(db: &Db, id: i64) -> DbResult<Option<Loan>> {
    db.conn()
        .query_row(
            &format!("SELECT {L_COLS} FROM loan WHERE id=?1"),
            rusqlite::params![id],
            map_loan,
        )
        .optional()
        .map_err(Into::into)
}

pub fn loan_save(db: &Db, l: &mut Loan) -> DbResult<i64> {
    let id = if l.id > 0 {
        db.conn().execute(
            "UPDATE loan SET kind=?2, no=?3, bank=?4, principal=?5, start_date=?6, end_date=?7,
             status=?8, rate_pct=?9, memo=?10 WHERE id=?1",
            rusqlite::params![
                l.id, l.kind, l.no, l.bank, l.principal.to_string(),
                l.start_date.format("%Y-%m-%d").to_string(),
                l.end_date.format("%Y-%m-%d").to_string(),
                l.status, l.rate_pct.to_string(), l.memo
            ],
        )?;
        l.id
    } else {
        db.conn().execute(
            "INSERT INTO loan(kind,no,bank,principal,start_date,end_date,status,rate_pct,memo,created_by,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            rusqlite::params![
                l.kind, l.no, l.bank, l.principal.to_string(),
                l.start_date.format("%Y-%m-%d").to_string(),
                l.end_date.format("%Y-%m-%d").to_string(),
                l.status, l.rate_pct.to_string(), l.memo, l.created_by, now()
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    l.id = id;
    Ok(id)
}

pub fn loan_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM loan WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 结清融资
pub fn loan_settle(db: &Db, id: i64) -> DbResult<()> {
    let mut l = loan_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("融资"))?;
    l.status = "settled".to_string();
    loan_save(db, &mut l)?;
    Ok(())
}

// ===========================================================================
// 资金日报 / 资金预测
// ===========================================================================

/// 单个现金/银行科目的资金日报行
#[derive(Clone, Debug, serde::Serialize)]
pub struct FundsDailyRow {
    pub account_code: String,
    pub account_name: String,
    /// 期初结存（带符号，借方为正）
    pub begin: Money,
    /// 本期收入
    pub income: Money,
    /// 本期支出
    pub expense: Money,
    /// 期末结存
    pub end: Money,
}

/// 资金日报：各现金/银行科目的期初、收入、支出、结存
pub fn funds_daily(db: &Db, period: Period) -> DbResult<Vec<FundsDailyRow>> {
    let accounts = crate::accounts::list(db)?;
    let cash_bank: Vec<_> = accounts
        .iter()
        .filter(|a| a.is_cash || a.is_bank)
        .collect();
    let snap = crate::balances::BalanceSnapshot::load(
        db,
        &crate::balances::BalanceQuery::period(period),
    )?;
    let mut out = Vec::new();
    for a in cash_bank {
        let row = snap.for_account(&a.code, None);
        out.push(FundsDailyRow {
            account_code: a.code.clone(),
            account_name: a.name.clone(),
            begin: row.begin,
            income: row.debit,
            expense: row.credit,
            end: row.end(),
        });
    }
    Ok(out)
}

/// 资金预测（头寸）
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct FundsForecast {
    /// 现金/银行结存
    pub cash_balance: Money,
    /// 在库应收票据
    pub receivable_bills: Money,
    /// 在库应付票据
    pub payable_bills: Money,
    /// 放款（可收回）
    pub lend: Money,
    /// 借款（需偿还）
    pub borrow: Money,
    /// 预计可用资金头寸
    pub position: Money,
}

/// 资金预测：结存 + 应收票据 − 应付票据 + 放款 − 借款
pub fn funds_forecast(db: &Db, period: Period) -> DbResult<FundsForecast> {
    let daily = funds_daily(db, period)?;
    let cash_balance: Money = daily.iter().map(|d| d.end).sum();

    let mut fc = FundsForecast {
        cash_balance,
        ..Default::default()
    };
    for b in bill_list(db, None)? {
        if b.status != BillStatus::InHand.code() {
            continue;
        }
        if b.kind == "receivable" {
            fc.receivable_bills += b.amount;
        } else {
            fc.payable_bills += b.amount;
        }
    }
    for l in loan_list(db, None)? {
        if l.status != "active" {
            continue;
        }
        if l.kind == "lend" {
            fc.lend += l.principal;
        } else {
            fc.borrow += l.principal;
        }
    }
    fc.position = cash_balance + fc.receivable_bills - fc.payable_bills + fc.lend - fc.borrow;
    Ok(fc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_fund_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }
    fn d(y: i32, mo: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, mo, dd).unwrap()
    }
    fn mon(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    #[test]
    fn bill_flow_and_forecast() {
        let db = tmpdb("bill");
        let p = Period::new(2026, 1).unwrap();
        let mut b = Bill {
            id: 0, kind: "receivable".into(), no: "PJ001".into(), period: p,
            issue_date: d(2026, 1, 5), due_date: d(2026, 4, 5),
            counterpart: "客户甲".into(), bank: "工行".into(), amount: mon("10000"),
            status: "in_hand".into(), handled_date: None, memo: String::new(),
            created_by: "u".into(), created_at: String::new(),
        };
        bill_save(&db, &mut b).unwrap();
        assert_eq!(bill_list(&db, None).unwrap().len(), 1);

        // 背书后不再计入应收票据
        bill_transition(&db, b.id, BillStatus::Endorsed, d(2026, 2, 1)).unwrap();
        let fc = funds_forecast(&db, p).unwrap();
        assert_eq!(fc.receivable_bills, mon("0"));

        // 已背书不可再流转
        assert!(bill_transition(&db, b.id, BillStatus::Discounted, d(2026, 2, 2)).is_err());
    }

    #[test]
    fn loan_signed_and_settle() {
        let db = tmpdb("loan");
        let mut l = Loan {
            id: 0, kind: "borrow".into(), no: "DK001".into(), bank: "建行".into(),
            principal: mon("500000"), rate_pct: mon("4.35"),
            start_date: d(2026, 1, 1), end_date: d(2027, 1, 1),
            status: "active".into(), memo: String::new(),
            created_by: "u".into(), created_at: String::new(),
        };
        loan_save(&db, &mut l).unwrap();
        assert!(l.signed().is_negative());
        loan_settle(&db, l.id).unwrap();
        assert_eq!(loan_get(&db, l.id).unwrap().unwrap().status, "settled");
    }

    #[test]
    fn funds_daily_sums_cash_bank() {
        let db = tmpdb("daily");
        let p = Period::new(2026, 1).unwrap();
        let rows = funds_daily(&db, p).unwrap();
        // 内置科目表里 1001 现金 / 1002 银行存款 标记了 is_cash / is_bank
        assert!(rows.iter().any(|r| r.account_code == "1001"));
        assert!(rows.iter().any(|r| r.account_code.starts_with("1002")));
    }
}
