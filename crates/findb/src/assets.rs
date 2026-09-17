//! 固定资产卡片与折旧明细
//!
//! 卡片只存**静态属性**（原值、年限、方法、开始期间），每月折旧额不落库也能算出来；
//! 但为了能追溯"某张凭证提了哪个月的折旧"，折旧明细仍然落一条记录。

use chrono::NaiveDate;
use fincore::engine::depreciation::{DepInput, DepMethod};
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

/// 资产状态
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssetStatus {
    /// 在用
    InUse,
    /// 停用 / 未使用
    Idle,
    /// 已清理
    Disposed,
}

impl AssetStatus {
    pub fn label(self) -> &'static str {
        match self {
            AssetStatus::InUse => "在用",
            AssetStatus::Idle => "停用",
            AssetStatus::Disposed => "已清理",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            AssetStatus::InUse => "in_use",
            AssetStatus::Idle => "idle",
            AssetStatus::Disposed => "disposed",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "idle" => AssetStatus::Idle,
            "disposed" => AssetStatus::Disposed,
            _ => AssetStatus::InUse,
        }
    }
    pub const ALL: &'static [AssetStatus] = &[
        AssetStatus::InUse,
        AssetStatus::Idle,
        AssetStatus::Disposed,
    ];
}

/// 固定资产卡片
#[derive(Clone, Debug)]
pub struct Asset {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub category: String,
    pub spec: String,
    pub dept: String,
    /// 资产科目（默认 1601 固定资产）
    pub asset_account: String,
    /// 累计折旧科目（默认 1602）
    pub dep_account: String,
    /// 折旧费用科目（默认 6602 管理费用）
    pub expense_account: String,
    pub original_value: Money,
    pub residual_rate: Money,
    pub life_months: i32,
    pub method: DepMethod,
    pub start_period: Period,
    pub disposed_period: Option<Period>,
    pub dispose_amount: Option<Money>,
    pub status: AssetStatus,
    pub voucher_id: Option<i64>,
    pub memo: String,
}

impl Asset {
    /// 转成引擎输入并做业务校验
    pub fn dep_input(&self) -> Result<DepInput, fincore::FinError> {
        let input = DepInput {
            original: self.original_value,
            residual_rate: self.residual_rate,
            life_months: self.life_months,
            method: self.method,
        };
        input.validate()?;
        Ok(input)
    }
    /// 已计提期数（含本期）
    pub fn elapsed_months(&self, at: Period) -> i32 {
        if at.ymm() < self.start_period.ymm() {
            return 0;
        }
        (at.year() - self.start_period.year()) * 12
            + (at.month() as i32 - self.start_period.month() as i32)
            + 1
    }
    /// 该期是否还应计提（停用不计提、清理当月仍提、超过年限不再提）
    pub fn should_depreciate(&self, at: Period) -> bool {
        if self.status == AssetStatus::Disposed {
            return false;
        }
        if self.status == AssetStatus::Idle {
            return false;
        }
        if at.ymm() < self.start_period.ymm() {
            return false;
        }
        if let Some(d) = self.disposed_period {
            // 清理当月照提，次月停提
            if at.ymm() > d.ymm() {
                return false;
            }
        }
        self.elapsed_months(at) <= self.life_months
    }
}

/// 折旧明细（每月一条）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DepRecord {
    pub id: i64,
    pub asset_id: i64,
    pub period: Period,
    pub amount: Money,
    pub accum: Money,
    pub net_value: Money,
    pub voucher_id: Option<i64>,
}

fn map_asset(r: &rusqlite::Row) -> rusqlite::Result<Asset> {
    let dp: Option<i64> = r.get(14)?;
    let da: Option<String> = r.get(15)?;
    Ok(Asset {
        id: r.get(0)?,
        code: r.get(1)?,
        name: r.get(2)?,
        category: r.get(3)?,
        spec: r.get(4)?,
        dept: r.get(5)?,
        asset_account: r.get(6)?,
        dep_account: r.get(7)?,
        expense_account: r.get(8)?,
        original_value: Money::parse_or_zero(&r.get::<_, String>(9)?),
        residual_rate: Money::parse_or_zero(&r.get::<_, String>(10)?),
        life_months: r.get(11)?,
        method: DepMethod::parse(&r.get::<_, String>(12)?),
        start_period: Period::from_ymm(r.get(13)?),
        disposed_period: dp.map(|v| Period::from_ymm(v as i32)),
        dispose_amount: da.map(|s| Money::parse_or_zero(&s)),
        status: AssetStatus::parse(&r.get::<_, String>(16)?),
        voucher_id: r.get(17)?,
        memo: r.get(18)?,
    })
}

const COLS: &str = "id,code,name,category,spec,dept,asset_account,dep_account,expense_account,
     original_value,residual_rate,life_months,method,start_period,disposed_period,dispose_amount,
     status,voucher_id,memo";

pub fn list(db: &Db) -> DbResult<Vec<Asset>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {COLS} FROM fixed_asset ORDER BY code"))?;
    let rows = st.query_map([], map_asset)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 按状态过滤
pub fn list_by_status(db: &Db, status: AssetStatus) -> DbResult<Vec<Asset>> {
    Ok(list(db)?.into_iter().filter(|a| a.status == status).collect())
}

/// 某期间应计提的卡片
pub fn active_at(db: &Db, period: Period) -> DbResult<Vec<Asset>> {
    Ok(list(db)?
        .into_iter()
        .filter(|a| a.should_depreciate(period))
        .collect())
}

pub fn get(db: &Db, id: i64) -> DbResult<Option<Asset>> {
    db.conn()
        .query_row(
            &format!("SELECT {COLS} FROM fixed_asset WHERE id=?1"),
            rusqlite::params![id],
            map_asset,
        )
        .optional()
        .map_err(Into::into)
}

pub fn get_by_code(db: &Db, code: &str) -> DbResult<Option<Asset>> {
    db.conn()
        .query_row(
            &format!("SELECT {COLS} FROM fixed_asset WHERE code=?1"),
            rusqlite::params![code],
            map_asset,
        )
        .optional()
        .map_err(Into::into)
}

/// 生成下一个资产编码：`GD` + 4 位序号
pub fn next_code(db: &Db) -> DbResult<String> {
    let max: i64 = db
        .conn()
        .query_row(
            "SELECT COALESCE(MAX(CAST(SUBSTR(code,3) AS INTEGER)),0) FROM fixed_asset
             WHERE code GLOB 'GD[0-9]*'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(format!("GD{:04}", max + 1))
}

pub fn insert(db: &Db, a: &Asset) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO fixed_asset(code,name,category,spec,dept,asset_account,dep_account,
            expense_account,original_value,residual_rate,life_months,method,start_period,
            disposed_period,dispose_amount,status,voucher_id,memo)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        rusqlite::params![
            a.code,
            a.name,
            a.category,
            a.spec,
            a.dept,
            a.asset_account,
            a.dep_account,
            a.expense_account,
            crate::money_param(a.original_value),
            crate::exact_param(a.residual_rate),
            a.life_months,
            a.method.code(),
            a.start_period.ymm(),
            a.disposed_period.map(|p| p.ymm()),
            a.dispose_amount.map(crate::money_param),
            a.status.code(),
            a.voucher_id,
            a.memo
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn update(db: &Db, a: &Asset) -> DbResult<()> {
    update_on(db.conn(), a)
}

/// 同 `update`，但只依赖连接，可在调用方的事务内执行
pub fn update_on(conn: &rusqlite::Connection, a: &Asset) -> DbResult<()> {
    conn.execute(
        "UPDATE fixed_asset SET code=?2,name=?3,category=?4,spec=?5,dept=?6,asset_account=?7,
            dep_account=?8,expense_account=?9,original_value=?10,residual_rate=?11,life_months=?12,
            method=?13,start_period=?14,disposed_period=?15,dispose_amount=?16,status=?17,
            voucher_id=?18,memo=?19 WHERE id=?1",
        rusqlite::params![
            a.id,
            a.code,
            a.name,
            a.category,
            a.spec,
            a.dept,
            a.asset_account,
            a.dep_account,
            a.expense_account,
            crate::money_param(a.original_value),
            crate::exact_param(a.residual_rate),
            a.life_months,
            a.method.code(),
            a.start_period.ymm(),
            a.disposed_period.map(|p| p.ymm()),
            a.dispose_amount.map(crate::money_param),
            a.status.code(),
            a.voucher_id,
            a.memo
        ],
    )?;
    Ok(())
}

/// 删除卡片。已提过折旧的卡片不允许直接删，避免账实不符。
pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM asset_depreciation WHERE asset_id=?1",
        rusqlite::params![id],
        |r| r.get(0),
    )?;
    if n > 0 {
        return Err(fincore::FinError::msg(format!(
            "该卡片已计提 {n} 期折旧，不能删除。请走资产清理流程。"
        ))
        .into());
    }
    db.conn()
        .execute("DELETE FROM fixed_asset WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

// ---------------- 折旧明细 ----------------

fn map_dep(r: &rusqlite::Row) -> rusqlite::Result<DepRecord> {
    Ok(DepRecord {
        id: r.get(0)?,
        asset_id: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        amount: Money::parse_or_zero(&r.get::<_, String>(3)?),
        accum: Money::parse_or_zero(&r.get::<_, String>(4)?),
        net_value: Money::parse_or_zero(&r.get::<_, String>(5)?),
        voucher_id: r.get(6)?,
    })
}

pub fn dep_list(db: &Db, asset_id: i64) -> DbResult<Vec<DepRecord>> {
    let mut st = db.conn().prepare(
        "SELECT id,asset_id,period,amount,accum,net_value,voucher_id FROM asset_depreciation
         WHERE asset_id=?1 ORDER BY period",
    )?;
    let rows = st
        .query_map(rusqlite::params![asset_id], map_dep)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn dep_list_period(db: &Db, period: Period) -> DbResult<Vec<DepRecord>> {
    let mut st = db.conn().prepare(
        "SELECT id,asset_id,period,amount,accum,net_value,voucher_id FROM asset_depreciation
         WHERE period=?1 ORDER BY asset_id",
    )?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], map_dep)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn dep_of(db: &Db, asset_id: i64, period: Period) -> DbResult<Option<DepRecord>> {
    db.conn()
        .query_row(
            "SELECT id,asset_id,period,amount,accum,net_value,voucher_id FROM asset_depreciation
             WHERE asset_id=?1 AND period=?2",
            rusqlite::params![asset_id, period.ymm()],
            map_dep,
        )
        .optional()
        .map_err(Into::into)
}

/// 写入 / 覆盖某期折旧（幂等，重复计提不会产生两条）
pub fn dep_upsert(db: &Db, r: &DepRecord) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO asset_depreciation(asset_id,period,amount,accum,net_value,voucher_id)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(asset_id,period) DO UPDATE SET
            amount=excluded.amount, accum=excluded.accum,
            net_value=excluded.net_value, voucher_id=excluded.voucher_id",
        rusqlite::params![
            r.asset_id,
            r.period.ymm(),
            crate::money_param(r.amount),
            crate::money_param(r.accum),
            crate::money_param(r.net_value),
            r.voucher_id
        ],
    )?;
    Ok(())
}

/// 删除某期折旧（重新计提 / 反结账时用）
pub fn dep_delete_period(db: &Db, period: Period) -> DbResult<usize> {
    let n = db.conn().execute(
        "DELETE FROM asset_depreciation WHERE period=?1",
        rusqlite::params![period.ymm()],
    )?;
    Ok(n)
}

/// 截至某期前（不含该期）的累计折旧
pub fn accum_before(db: &Db, asset_id: i64, period: Period) -> DbResult<Money> {
    let s: Option<String> = db
        .conn()
        .query_row(
            "SELECT accum FROM asset_depreciation WHERE asset_id=?1 AND period<?2
             ORDER BY period DESC LIMIT 1",
            rusqlite::params![asset_id, period.ymm()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(s.map(|x| Money::parse_or_zero(&x)).unwrap_or(Money::ZERO))
}

/// 计算某卡片在某期的应计折旧（不落库，给预览用）
pub fn planned_dep(a: &Asset, period: Period) -> DbResult<Option<Money>> {
    if !a.should_depreciate(period) {
        return Ok(None);
    }
    let input = a.dep_input()?;
    let seq = a.elapsed_months(period);
    let rows = fincore::engine::depreciation::schedule(&input)?;
    Ok(rows.get((seq - 1) as usize).map(|r| r.amount))
}

/// 资产台账：卡片 + 累计折旧 + 净值
#[derive(Clone, Debug)]
pub struct AssetLedgerRow {
    pub asset: Asset,
    pub accum: Money,
    pub net: Money,
    pub months: i32,
}

pub fn ledger(db: &Db, at: Period) -> DbResult<Vec<AssetLedgerRow>> {
    let mut out = Vec::new();
    for a in list(db)? {
        // 优先取落库值（可能与理论值不同，比如手工调整过），没有再算
        let (accum, months) = match dep_of(db, a.id, at)? {
            Some(r) => (r.accum, a.elapsed_months(at)),
            None => {
                let input = match a.dep_input() {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                let seq = a.elapsed_months(at);
                let m = seq.min(a.life_months).max(0);
                let rows = fincore::engine::depreciation::schedule(&input)?;
                let acc = rows
                    .get((m as usize).saturating_sub(1))
                    .map(|r| r.accum)
                    .unwrap_or_else(|| accum_before(db, a.id, at).unwrap_or(Money::ZERO));
                (acc, m)
            }
        };
        let net = a.original_value - accum;
        out.push(AssetLedgerRow {
            asset: a,
            accum,
            net,
            months,
        });
    }
    Ok(out)
}

/// 资产清理：标记状态并删除清理期之后的折旧记录（同一事务，避免半更新）
pub fn dispose(db: &Db, id: i64, period: Period, amount: Money) -> DbResult<()> {
    let mut a = match get(db, id)? {
        Some(a) => a,
        None => return Err(fincore::FinError::not_found("资产卡片").into()),
    };
    a.status = AssetStatus::Disposed;
    a.disposed_period = Some(period);
    a.dispose_amount = Some(amount);
    let tx = db.write_tx()?;
    update_on(&tx, &a)?;
    tx.execute(
        "DELETE FROM asset_depreciation WHERE asset_id=?1 AND period>?2",
        rusqlite::params![id, period.ymm()],
    )?;
    tx.commit()?;
    Ok(())
}

/// 解析业务日期（资产模块只在导入 CSV 时用得到）
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .or_else(|| NaiveDate::parse_from_str(s, "%Y/%m/%d").ok())
}

// ===========================================================================
// 资产类别 / 减值 / 附属设备 / 盘点
// ===========================================================================

/// 资产类别清单（去重）
pub fn categories(db: &Db) -> DbResult<Vec<String>> {
    let mut st = db.conn().prepare(
        "SELECT DISTINCT category FROM fixed_asset WHERE category <> '' ORDER BY category",
    )?;
    let rows = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 资产减值：记录减值金额
pub fn impair(db: &Db, asset_id: i64, period: Period, amount: Money, memo: &str) -> DbResult<i64> {
    if amount.is_zero() {
        return Err(fincore::FinError::msg("减值金额不能为 0").into());
    }
    db.conn().execute(
        "INSERT INTO asset_impairment(asset_id,period,amount,memo) VALUES(?1,?2,?3,?4)",
        rusqlite::params![asset_id, period.ymm(), crate::money_param(amount), memo],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn impairment_sum(db: &Db, asset_id: i64) -> DbResult<Money> {
    let mut st = db.conn().prepare("SELECT amount FROM asset_impairment WHERE asset_id=?1")?;
    let rows = st
        .query_map([asset_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| Money::parse_or_zero(s)).sum())
}

/// 附属设备
#[derive(Clone, Debug)]
pub struct Accessory {
    pub id: i64,
    pub asset_id: i64,
    pub name: String,
    pub spec: String,
    pub qty: i32,
    pub memo: String,
}

pub fn accessory_add(db: &Db, asset_id: i64, name: &str, spec: &str, qty: i32, memo: &str) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO asset_accessory(asset_id,name,spec,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![asset_id, name, spec, qty, memo],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn accessory_list(db: &Db, asset_id: i64) -> DbResult<Vec<Accessory>> {
    let mut st = db.conn().prepare(
        "SELECT id,asset_id,name,spec,qty,memo FROM asset_accessory WHERE asset_id=?1 ORDER BY id",
    )?;
    let rows = st
        .query_map([asset_id], |r| {
            Ok(Accessory {
                id: r.get(0)?,
                asset_id: r.get(1)?,
                name: r.get(2)?,
                spec: r.get(3)?,
                qty: r.get(4)?,
                memo: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn accessory_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn().execute("DELETE FROM asset_accessory WHERE id=?1", [id])?;
    Ok(())
}

/// 资产盘点单
#[derive(Clone, Debug)]
pub struct AssetCount {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    pub status: String, // draft / posted
    pub prepared_by: String,
    pub memo: String,
    /// (asset_id, found) 明细
    pub lines: Vec<(i64, bool, String)>,
}

pub fn ac_next_no(db: &Db, period: Period) -> DbResult<String> {
    let prefix = format!("ZCPD{:04}{:02}", period.year(), period.month());
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM asset_count WHERE no LIKE ?1",
        rusqlite::params![format!("{prefix}%")],
        |r| r.get(0),
    )?;
    Ok(format!("{prefix}-{:03}", n + 1))
}

pub fn ac_save(db: &Db, c: &mut AssetCount) -> DbResult<i64> {
    let tx = db.write_tx()?;
    let id = if c.id > 0 {
        tx.execute(
            "UPDATE asset_count SET period=?2, date=?3, status=?4, prepared_by=?5, memo=?6 WHERE id=?1",
            rusqlite::params![c.id, c.period.ymm(), c.date.format("%Y-%m-%d").to_string(), c.status, c.prepared_by, c.memo],
        )?;
        c.id
    } else {
        tx.execute(
            "INSERT INTO asset_count(no,period,date,status,prepared_by,memo) VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![c.no, c.period.ymm(), c.date.format("%Y-%m-%d").to_string(), c.status, c.prepared_by, c.memo],
        )?;
        tx.last_insert_rowid()
    };
    tx.execute("DELETE FROM asset_count_line WHERE ac_id=?1", [id])?;
    for (asset_id, found, memo) in &c.lines {
        tx.execute(
            "INSERT INTO asset_count_line(ac_id,asset_id,found,memo) VALUES(?1,?2,?3,?4)",
            rusqlite::params![id, asset_id, if *found { 1 } else { 0 }, memo],
        )?;
    }
    tx.commit()?;
    c.id = id;
    Ok(id)
}

/// 盘点过账：盘亏（found=false）的资产标记为 Idle 并记录
pub fn ac_post(db: &Db, id: i64) -> DbResult<usize> {
    let mut st = db.conn().prepare(
        "SELECT asset_id, found FROM asset_count_line WHERE ac_id=?1",
    )?;
    let lines: Vec<(i64, bool)> = st
        .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? != 0)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut n = 0;
    let tx = db.write_tx()?;
    for (asset_id, found) in lines {
        if !found {
            if let Some(mut a) = get(db, asset_id)? {
                if a.status != AssetStatus::Disposed {
                    a.status = AssetStatus::Idle;
                    a.memo = format!("{} 盘亏", a.memo);
                    update(db, &a)?;
                    n += 1;
                }
            }
        }
    }
    tx.execute("UPDATE asset_count SET status='posted' WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_asset_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }

    fn asset(code: &str, original: &str, months: i32) -> Asset {
        Asset {
            id: 0,
            code: code.into(),
            name: "测试设备".into(),
            category: "电子设备".into(),
            spec: String::new(),
            dept: String::new(),
            asset_account: "1601".into(),
            dep_account: "1602".into(),
            expense_account: "6602".into(),
            original_value: Money::parse(original).unwrap(),
            residual_rate: Money::parse("0.05").unwrap(),
            life_months: months,
            method: DepMethod::Straight,
            start_period: Period::new(2026, 1).unwrap(),
            disposed_period: None,
            dispose_amount: None,
            status: AssetStatus::InUse,
            voucher_id: None,
            memo: String::new(),
        }
    }

    #[test]
    fn crud_and_dep() {
        let db = tmpdb("crud");
        let id = insert(&db, &asset("GD0001", "12000", 12)).unwrap();
        let a = get(&db, id).unwrap().unwrap();
        assert_eq!(a.original_value, Money::parse("12000").unwrap());
        assert_eq!(a.method, DepMethod::Straight);

        let p = Period::new(2026, 1).unwrap();
        let amt = planned_dep(&a, p).unwrap().unwrap();
        // (12000 - 600) / 12 = 950
        assert_eq!(amt, Money::parse("950").unwrap());

        dep_upsert(
            &db,
            &DepRecord {
                id: 0,
                asset_id: id,
                period: p,
                amount: amt,
                accum: amt,
                net_value: a.original_value - amt,
                voucher_id: None,
            },
        )
        .unwrap();
        // 幂等
        dep_upsert(
            &db,
            &DepRecord {
                id: 0,
                asset_id: id,
                period: p,
                amount: amt,
                accum: amt,
                net_value: a.original_value - amt,
                voucher_id: None,
            },
        )
        .unwrap();
        assert_eq!(dep_list(&db, id).unwrap().len(), 1);

        // 已提折旧不能删卡片
        assert!(delete(&db, id).is_err());
    }

    #[test]
    fn elapsed_and_should() {
        let db = tmpdb("elapsed");
        let id = insert(&db, &asset("GD0002", "6000", 6)).unwrap();
        let mut a = get(&db, id).unwrap().unwrap();
        let p1 = Period::new(2026, 1).unwrap();
        let p7 = Period::new(2026, 7).unwrap();
        assert_eq!(a.elapsed_months(p1), 1);
        assert_eq!(a.elapsed_months(p7), 7);
        assert!(!a.should_depreciate(p7)); // 超过 6 个月不再提
        assert!(a.should_depreciate(Period::new(2026, 6).unwrap()));

        a.status = AssetStatus::Idle;
        assert!(!a.should_depreciate(p1));

        // 清理当月仍提，次月停
        a.status = AssetStatus::InUse;
        a.disposed_period = Some(p1);
        assert!(a.should_depreciate(p1));
        assert!(!a.should_depreciate(Period::new(2026, 2).unwrap()));
    }

    #[test]
    fn next_code_seq() {
        let db = tmpdb("code");
        assert_eq!(next_code(&db).unwrap(), "GD0001");
        insert(&db, &asset("GD0001", "1000", 12)).unwrap();
        assert_eq!(next_code(&db).unwrap(), "GD0002");
    }

    #[test]
    fn impairment_accessory_count() {
        let db = tmpdb("imp");
        let id = insert(&db, &asset("GD0001", "10000", 60)).unwrap();
        // 减值 2000
        impair(&db, id, Period::new(2026, 2).unwrap(), Money::parse("2000").unwrap(), "减值测试").unwrap();
        assert_eq!(impairment_sum(&db, id).unwrap(), Money::parse("2000").unwrap());
        // 类别
        assert_eq!(categories(&db).unwrap(), vec!["电子设备".to_string()]);
        // 附属设备
        accessory_add(&db, id, "显卡", "RTX", 2, "").unwrap();
        assert_eq!(accessory_list(&db, id).unwrap().len(), 1);
        // 盘点：盘亏 → 过账后资产变 Idle
        let p = Period::new(2026, 2).unwrap();
        let mut c = AssetCount {
            id: 0, no: ac_next_no(&db, p).unwrap(), period: p,
            date: NaiveDate::from_ymd_opt(2026, 2, 28).unwrap(),
            status: "draft".into(), prepared_by: "张三".into(), memo: String::new(),
            lines: vec![(id, false, "盘亏".to_string())],
        };
        let cid = ac_save(&db, &mut c).unwrap();
        assert_eq!(ac_post(&db, cid).unwrap(), 1);
        assert_eq!(get(&db, id).unwrap().unwrap().status, AssetStatus::Idle);
    }
}
