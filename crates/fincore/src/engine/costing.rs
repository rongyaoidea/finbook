//! 存货成本核算
//!
//! 三种计价方式，核心难点都在**出库成本**怎么定：
//!
//! | 方法 | 出库单价来源 |
//! |------|-------------|
//! | 移动加权平均 | 每次入库后重算：`(原金额 + 本次金额) / (原数量 + 本次数量)` |
//! | 先进先出 FIFO | 按最早批次依次消耗，批内单价不同 |
//! | 个别计价 | 指定批次（凭证分录直接带单价，不走本模块） |
//!
//! 两个必须处理好的边界：
//! 1. **负库存**：先出库后入库时会出现。本模块允许负数量但**单价取上一次已知成本**，
//!    等入库时再冲回差额——实务上叫"暂估"，这里简化处理但保证金额守恒。
//! 2. **尾差**：最后一次出库把结存金额清零，误差挤到当期成本，不留在结存里。

use crate::money::{Money, QTY_DP};
use crate::FinError;

/// 计价方式
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CostMethod {
    /// 移动加权平均
    #[default]
    MovingAverage,
    /// 先进先出
    Fifo,
}

impl CostMethod {
    pub fn label(&self) -> &'static str {
        match self {
            CostMethod::MovingAverage => "移动加权平均",
            CostMethod::Fifo => "先进先出",
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            CostMethod::MovingAverage => "moving_average",
            CostMethod::Fifo => "fifo",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "fifo" => CostMethod::Fifo,
            _ => CostMethod::MovingAverage,
        }
    }
    pub const ALL: &'static [CostMethod] = &[CostMethod::MovingAverage, CostMethod::Fifo];
}

/// 出入库流水（正数入库、负数出库）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Move {
    /// 数量，正=入库 负=出库
    pub qty: Money,
    /// 入库时的单价；出库时可为 None（由系统按计价方式算）
    pub price: Option<Money>,
}

/// 一批存货（FIFO 用）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lot {
    pub qty: Money,
    pub unit_cost: Money,
}

/// 单个存货的结存
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StockState {
    pub qty: Money,
    /// 结存金额（不含税成本）
    pub amount: Money,
    /// FIFO 的批次队列（最早在前）
    pub lots: Vec<Lot>,
    /// 上一次已知单价（负库存出库时用）
    last_price: Money,
}

impl StockState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前结存单价（零数量时为 0，避免除零）
    pub fn unit_cost(&self) -> Money {
        if self.qty.is_zero() {
            return Money::ZERO;
        }
        (self.amount / self.qty).round_dp(QTY_DP + 2)
    }

    /// 应用一条流水，返回本次出库成本（入库返回 None）
    pub fn apply(&mut self, mv: &Move, method: CostMethod) -> Result<Option<Money>, FinError> {
        if mv.qty.is_zero() {
            return Ok(None);
        }
        if mv.qty.is_positive() {
            self.apply_in(mv, method)?;
            Ok(None)
        } else {
            let cost = self.apply_out(mv, method)?;
            Ok(Some(cost))
        }
    }

    fn apply_in(&mut self, mv: &Move, _method: CostMethod) -> Result<(), FinError> {
        let qty = mv.qty;
        // 入库必须有单价
        let price = match mv.price {
            Some(p) if p >= Money::ZERO => p,
            _ => {
                // 没给单价就按当前结存价入账，避免金额凭空消失
                if self.qty.is_zero() {
                    return Err(FinError::msg("首次入库必须指定单价"));
                }
                self.unit_cost()
            }
        };
        let amount = (qty * price).round2();

        // 负库存补回：先把负数量填平，这部分按 last_price 冲回
        if self.qty.is_negative() {
            let fill = (-self.qty).min(qty);
            let back = (fill * self.last_price).round2();
            self.qty = self.qty + fill;
            self.amount = self.amount + back;
            let rest = qty - fill;
            if rest > Money::ZERO {
                self.qty = self.qty + rest;
                self.amount = self.amount + (rest * price).round2();
                self.last_price = price;
                self.push_lot(rest, price);
            }
            return Ok(());
        }

        self.qty = self.qty + qty;
        self.amount = self.amount + amount;
        self.last_price = price;
        self.push_lot(qty, price);
        Ok(())
    }

    fn push_lot(&mut self, qty: Money, cost: Money) {
        // 同价批次合并，避免批次列表无限膨胀
        if let Some(last) = self.lots.last_mut() {
            if last.unit_cost == cost {
                last.qty = last.qty + qty;
                return;
            }
        }
        self.lots.push(Lot {
            qty,
            unit_cost: cost,
        });
    }

    fn apply_out(&mut self, mv: &Move, method: CostMethod) -> Result<Money, FinError> {
        let want = (-mv.qty).round_dp(QTY_DP); // 需要出库的正数量

        if let Some(p) = mv.price {
            // 指定单价（个别计价 / 手工调整），直接按它出库
            let cost = (want * p).round2();
            self.consume_lots(want);
            self.qty = self.qty - want;
            self.amount = self.amount - cost;
            self.normalize_if_empty();
            return Ok(cost);
        }

        let cost = match method {
            CostMethod::MovingAverage => {
                // 结存为零时用 last_price（先出库后入库的场景）
                let unit = if self.qty.is_zero() {
                    self.last_price
                } else {
                    self.unit_cost()
                };
                (want * unit).round2()
            }
            CostMethod::Fifo => self.cost_by_fifo(want),
        };

        self.consume_lots(want);
        self.qty = (self.qty - want).round_dp(QTY_DP);
        self.amount = self.amount - cost;
        self.normalize_if_empty();
        Ok(cost)
    }

    fn cost_by_fifo(&self, want: Money) -> Money {
        let mut remain = want;
        let mut cost = Money::ZERO;
        for lot in &self.lots {
            if remain <= Money::ZERO {
                break;
            }
            let take = remain.min(lot.qty);
            cost = cost + (take * lot.unit_cost).round2();
            remain = remain - take;
        }
        // 批次不够（负库存）：剩余部分按 last_price 计价
        if remain > Money::ZERO {
            cost = cost + (remain * self.last_price).round2();
        }
        cost.round2()
    }

    fn consume_lots(&mut self, want: Money) {
        let mut remain = want;
        let mut idx = 0;
        while idx < self.lots.len() && remain > Money::ZERO {
            let take = remain.min(self.lots[idx].qty);
            self.lots[idx].qty = self.lots[idx].qty - take;
            remain = remain - take;
            if self.lots[idx].qty.abs() < Money::new(rust_decimal::Decimal::new(1, QTY_DP)) {
                self.lots.remove(idx);
            } else {
                idx += 1;
            }
        }
    }

    /// 数量清零时把金额也抹平，尾差挤进当期成本（已在上一步从 amount 扣除）
    fn normalize_if_empty(&mut self) {
        if self.qty.abs() < Money::new(rust_decimal::Decimal::new(1, QTY_DP)) {
            self.qty = Money::ZERO;
            self.amount = Money::ZERO;
            self.lots.clear();
        }
    }
}

/// 一次性跑完一批流水，返回每条流水对应的出库成本（入库为 None）
///
/// `moves` 必须按业务日期升序。
pub fn run(
    moves: &[Move],
    method: CostMethod,
) -> Result<(Vec<Option<Money>>, StockState), FinError> {
    let mut st = StockState::new();
    let mut out = Vec::with_capacity(moves.len());
    for mv in moves {
        out.push(st.apply(mv, method)?);
    }
    Ok((out, st))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    fn q(s: &str) -> Money {
        Money::parse(s).unwrap().round_dp(QTY_DP)
    }

    #[test]
    fn moving_average_reprices_on_each_purchase() {
        let moves = vec![
            Move { qty: q("100"), price: Some(m("10")) },   // 入 100 @10 → 1000
            Move { qty: q("50"), price: Some(m("12")) },    // 入 50 @12 → 累计 150 / 1600，均价 10.6667
            Move { qty: q("-80"), price: None },            // 出 80 @10.6667 = 853.33
        ];
        let (costs, st) = run(&moves, CostMethod::MovingAverage).unwrap();
        assert_eq!(costs[0], None);
        assert_eq!(costs[1], None);
        assert_eq!(costs[2], Some(m("853.33")));
        assert_eq!(st.qty, q("70"));
        assert_eq!(st.amount, m("746.67"));
    }

    #[test]
    fn fifo_consumes_oldest_first() {
        let moves = vec![
            Move { qty: q("100"), price: Some(m("10")) },
            Move { qty: q("50"), price: Some(m("12")) },
            Move { qty: q("-80"), price: None }, // 先吃 100@10 的 80 → 800
        ];
        let (costs, st) = run(&moves, CostMethod::Fifo).unwrap();
        assert_eq!(costs[2], Some(m("800")));
        assert_eq!(st.qty, q("70")); // 20@10 + 50@12
        assert_eq!(st.amount, m("800"));
        assert_eq!(st.lots.len(), 2);
    }

    #[test]
    fn fifo_crosses_lots() {
        let moves = vec![
            Move { qty: q("10"), price: Some(m("5")) },
            Move { qty: q("10"), price: Some(m("7")) },
            Move { qty: q("-15"), price: None }, // 10@5 + 5@7 = 50 + 35 = 85
        ];
        let (costs, _) = run(&moves, CostMethod::Fifo).unwrap();
        assert_eq!(costs[2], Some(m("85")));
    }

    #[test]
    fn sell_out_clears_to_zero() {
        let moves = vec![
            Move { qty: q("10"), price: Some(m("3.33")) },
            Move { qty: q("-10"), price: None },
        ];
        let (_, st) = run(&moves, CostMethod::MovingAverage).unwrap();
        assert_eq!(st.qty, Money::ZERO);
        assert_eq!(st.amount, Money::ZERO); // 尾差已被抹平，不残留
        assert!(st.lots.is_empty());
    }

    #[test]
    fn negative_stock_recovers_on_purchase() {
        // 先卖后买：出库 10（此时无库存，按 0 计价），再入库 10 @8
        let moves = vec![
            Move { qty: q("-10"), price: None },
            Move { qty: q("10"), price: Some(m("8")) },
        ];
        let (costs, st) = run(&moves, CostMethod::MovingAverage).unwrap();
        assert_eq!(costs[0], Some(Money::ZERO));
        assert_eq!(st.qty, Money::ZERO);
        assert_eq!(st.amount, Money::ZERO);
    }

    #[test]
    fn first_in_must_have_price() {
        let moves = vec![Move { qty: q("10"), price: None }];
        assert!(run(&moves, CostMethod::MovingAverage).is_err());
    }

    #[test]
    fn method_parse_roundtrip() {
        for c in CostMethod::ALL {
            assert_eq!(CostMethod::parse(c.code()), *c);
        }
    }
}
