//! 可视化工作流（对标金蝶审批流设计器）
//!
//! 节点-动作模型：start / approve（审批）/ condition / message 节点 + normal / reject 连线，
//! 节点携带画布坐标 (x, y) 供前端 SVG 画布渲染。**无已发布流程 = 走原固定审批链**
//! （默认流，向后兼容：流程不配置时所有既有审批行为不变）。
//!
//! 运行时：单据的第一次审批动作自动创建实例并推进到下一节点；走到终点（无 normal
//! 出边的 approve 节点即流程终点）时返回 `Gate::Final`，由调用方执行其原有业务审批
//! （报价转已审批 / 请购批准 / 报销通过 / 收付款单出凭证）。驳回沿 reject 连线或节点
//! reject_to 移动，无路径则实例终态 rejected（业务单据状态由调用方自行处理）。

use fincore::{Perm, User};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::{Db, DbResult};

pub const BIZ_QUOTATION: &str = "quotation";
pub const BIZ_PURCHASE_REQ: &str = "purchase_req";
pub const BIZ_CLAIM: &str = "claim";
pub const BIZ_RECEIPT: &str = "receipt";

/// 业务类型 → 中文（前端下拉与实例列表展示）
pub const ALL_BIZ: &[(&str, &str)] = &[
    (BIZ_QUOTATION, "报价单"),
    (BIZ_PURCHASE_REQ, "请购单"),
    (BIZ_CLAIM, "报销单"),
    (BIZ_RECEIPT, "收付款单"),
];

pub fn biz_label(t: &str) -> String {
    ALL_BIZ
        .iter()
        .find(|(k, _)| *k == t)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| t.to_string())
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn role_code(r: &Role) -> String {
    serde_json::to_value(r)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

use fincore::Role;

/// 画布节点
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WfNode {
    pub id: String,
    /// start / approve / condition / message
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub name: String,
    /// 允许审批的角色 code（空 = 任何具备审批权的人）
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default = "default_strategy")]
    pub strategy: String,
    #[serde(default)]
    pub reject_to: String,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
}

fn default_strategy() -> String {
    "all".to_string()
}

/// 画布连线
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WfEdge {
    pub id: String,
    #[serde(rename = "from")]
    pub from_node: String,
    #[serde(rename = "to")]
    pub to_node: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub condition: String,
}

fn default_kind() -> String {
    "normal".to_string()
}

/// 流程定义（含节点与连线）
#[derive(Clone, Debug, Serialize)]
pub struct WfFlow {
    pub id: i64,
    pub name: String,
    pub biz_type: String,
    /// draft / published
    pub status: String,
    pub nodes: Vec<WfNode>,
    pub edges: Vec<WfEdge>,
    pub created_by: String,
    pub updated_at: String,
}

/// 客户端保存入参（节点/连线来自 JSON）
#[derive(Clone, Debug, Deserialize)]
pub struct WfFlowInput {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    pub biz_type: String,
    pub nodes: Vec<WfNode>,
    pub edges: Vec<WfEdge>,
}

/// 运行轨迹条目
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WfLogEntry {
    pub node: String,
    pub action: String,
    pub who: String,
    pub at: String,
}

/// 运行实例（列表用）
#[derive(Clone, Debug, Serialize)]
pub struct WfInstance {
    pub id: i64,
    pub flow_id: i64,
    pub flow_name: String,
    pub biz_type: String,
    pub biz_label: String,
    pub biz_id: i64,
    pub current_node: String,
    pub current_label: String,
    /// running / approved / rejected
    pub status: String,
    pub log: Vec<WfLogEntry>,
    pub created_at: String,
}

/// 审批拦截结果
#[derive(Clone, Debug)]
pub enum Gate {
    /// 该类型没有已发布流程 → 调用方执行原有直接审批（默认流）
    NoFlow,
    /// 已推进到下一节点（未到终态）：调用方只回 pending 提示，不执行业务动作
    Pending { next: String },
    /// 实例到终态：调用方执行其原有业务审批/驳回
    Final { approved: bool },
}

fn load_nodes(tx: &rusqlite::Connection, flow_id: i64) -> DbResult<Vec<WfNode>> {
    let mut st = tx.prepare(
        "SELECT id,type,name,participants,strategy,reject_to,x,y
         FROM workflow_node WHERE flow_id=?1 ORDER BY seq, rowid",
    )?;
    let rows = st
        .query_map([flow_id], |r| {
            let p: String = r.get(3)?;
            Ok(WfNode {
                id: r.get(0)?,
                node_type: r.get(1)?,
                name: r.get(2)?,
                participants: serde_json::from_str(&p).unwrap_or_default(),
                strategy: r.get(4)?,
                reject_to: r.get(5)?,
                x: r.get(6)?,
                y: r.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn load_edges(tx: &rusqlite::Connection, flow_id: i64) -> DbResult<Vec<WfEdge>> {
    let mut st = tx.prepare(
        "SELECT id,from_node,to_node,kind,condition
         FROM workflow_edge WHERE flow_id=?1 ORDER BY seq, rowid",
    )?;
    let rows = st
        .query_map([flow_id], |r| {
            Ok(WfEdge {
                id: r.get(0)?,
                from_node: r.get(1)?,
                to_node: r.get(2)?,
                kind: r.get(3)?,
                condition: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn flow_of(tx: &rusqlite::Connection, id: i64) -> DbResult<Option<WfFlow>> {
    let row = tx
        .query_row(
            "SELECT id,name,biz_type,status,created_by,updated_at FROM workflow_flow WHERE id=?1",
            rusqlite::params![id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((id, name, biz_type, status, created_by, updated_at)) = row else {
        return Ok(None);
    };
    Ok(Some(WfFlow {
        id,
        name,
        biz_type,
        status,
        nodes: load_nodes(tx, id)?,
        edges: load_edges(tx, id)?,
        created_by,
        updated_at,
    }))
}

pub fn flow_list(db: &Db) -> DbResult<Vec<WfFlow>> {
    let mut st = db.conn().prepare(
        "SELECT id FROM workflow_flow ORDER BY CASE status WHEN 'published' THEN 0 ELSE 1 END, id DESC",
    )?;
    let ids: Vec<i64> = st
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for id in ids {
        if let Some(f) = flow_of(db.conn(), id)? {
            out.push(f);
        }
    }
    Ok(out)
}

/// 保存流程（新建或更新）：整体替换节点与连线；校验 start 恰好一个、连线端点存在。
pub fn flow_save(db: &Db, f: &WfFlowInput, who: &str) -> DbResult<i64> {
    if f.name.trim().is_empty() {
        return Err(fincore::FinError::validate("流程名称必填").into());
    }
    if ALL_BIZ.iter().all(|(k, _)| *k != f.biz_type) {
        return Err(fincore::FinError::validate("非法业务类型").into());
    }
    if f.nodes.is_empty() {
        return Err(fincore::FinError::validate("至少需要一个节点").into());
    }
    let starts = f.nodes.iter().filter(|n| n.node_type == "start").count();
    if starts != 1 {
        return Err(fincore::FinError::validate("开始节点必须恰好 1 个").into());
    }
    let mut ids: std::collections::HashSet<&str> =
        f.nodes.iter().map(|n| n.id.as_str()).collect();
    if ids.len() != f.nodes.len() {
        return Err(fincore::FinError::validate("节点 id 重复").into());
    }
    for e in &f.edges {
        if !ids.contains(e.from_node.as_str()) || !ids.contains(e.to_node.as_str()) {
            return Err(fincore::FinError::validate(format!("连线端点不存在：{} → {}", e.from_node, e.to_node)).into());
        }
    }
    for n in &f.nodes {
        if !n.reject_to.is_empty() && !ids.contains(n.reject_to.as_str()) {
            return Err(fincore::FinError::validate(format!("驳回目标节点不存在：{}", n.reject_to)).into());
        }
    }
    ids.clear();
    let tx = db.write_tx()?;
    let flow_id = if f.id > 0 {
        let n = tx.execute(
            "UPDATE workflow_flow SET name=?2, biz_type=?3, updated_at=?4 WHERE id=?1",
            rusqlite::params![f.id, f.name.trim(), f.biz_type, now()],
        )?;
        if n == 0 {
            return Err(fincore::FinError::not_found("流程不存在").into());
        }
        f.id
    } else {
        tx.execute(
            "INSERT INTO workflow_flow(name,biz_type,status,created_by,created_at,updated_at)
             VALUES(?1,?2,'draft',?3,?4,?4)",
            rusqlite::params![f.name.trim(), f.biz_type, who, now()],
        )?;
        tx.last_insert_rowid()
    };
    tx.execute("DELETE FROM workflow_node WHERE flow_id=?1", [flow_id])?;
    tx.execute("DELETE FROM workflow_edge WHERE flow_id=?1", [flow_id])?;
    for (i, n) in f.nodes.iter().enumerate() {
        tx.execute(
            "INSERT INTO workflow_node(id,flow_id,type,name,participants,strategy,reject_to,seq,x,y)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                n.id,
                flow_id,
                n.node_type,
                n.name,
                serde_json::to_string(&n.participants)?,
                n.strategy,
                n.reject_to,
                i as i64,
                n.x,
                n.y
            ],
        )?;
    }
    for (i, e) in f.edges.iter().enumerate() {
        tx.execute(
            "INSERT INTO workflow_edge(id,flow_id,from_node,to_node,kind,condition,seq)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![e.id, flow_id, e.from_node, e.to_node, e.kind, e.condition, i as i64],
        )?;
    }
    tx.commit()?;
    Ok(flow_id)
}

/// 发布 / 撤回发布（发布要求至少一个审批节点）
pub fn flow_set_status(db: &Db, id: i64, publish: bool, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    if publish {
        let nodes = load_nodes(&tx, id)?;
        if nodes.is_empty() {
            return Err(fincore::FinError::validate("流程不存在或没有节点").into());
        }
        if !nodes.iter().any(|n| n.node_type == "approve") {
            return Err(fincore::FinError::validate("发布前至少需要一个审批节点").into());
        }
    }
    let status = if publish { "published" } else { "draft" };
    let n = tx.execute(
        "UPDATE workflow_flow SET status=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![id, status, now()],
    )?;
    if n == 0 {
        return Err(fincore::FinError::not_found("流程不存在").into());
    }
    let _ = who;
    tx.commit()?;
    Ok(())
}

/// 删除流程（有运行中实例时拒绝）
pub fn flow_delete(db: &Db, id: i64) -> DbResult<()> {
    let tx = db.write_tx()?;
    let running: i64 = tx.query_row(
        "SELECT COUNT(*) FROM workflow_instance WHERE flow_id=?1 AND status='running'",
        [id],
        |r| r.get(0),
    )?;
    if running > 0 {
        return Err(
            fincore::FinError::state("该流程有运行中的审批实例，不能删除").into(),
        );
    }
    tx.execute("DELETE FROM workflow_instance WHERE flow_id=?1", [id])?;
    tx.execute("DELETE FROM workflow_edge WHERE flow_id=?1", [id])?;
    tx.execute("DELETE FROM workflow_node WHERE flow_id=?1", [id])?;
    let n = tx.execute("DELETE FROM workflow_flow WHERE id=?1", [id])?;
    if n == 0 {
        return Err(fincore::FinError::not_found("流程不存在").into());
    }
    tx.commit()?;
    Ok(())
}

/// 该业务类型已发布的流程（取最新发布）
pub fn published_flow_for(db: &Db, biz_type: &str) -> DbResult<Option<WfFlow>> {
    let id: Option<i64> = db
        .conn()
        .query_row(
            "SELECT id FROM workflow_flow WHERE biz_type=?1 AND status='published'
             ORDER BY id DESC LIMIT 1",
            [biz_type],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        Some(id) => flow_of(db.conn(), id),
        None => Ok(None),
    }
}

/// 流程全部实例（含当前节点中文名与轨迹）
pub fn instances(db: &Db) -> DbResult<Vec<WfInstance>> {
    let mut st = db.conn().prepare(
        "SELECT id,flow_id,biz_type,biz_id,current_node,status,log_json,created_at
         FROM workflow_instance ORDER BY id DESC LIMIT 500",
    )?;
    let raw: Vec<(i64, i64, String, i64, String, String, String, String)> = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut flows: std::collections::HashMap<i64, Option<WfFlow>> = std::collections::HashMap::new();
    let mut out = Vec::new();
    for (id, flow_id, biz_type, biz_id, current_node, status, log_json, created_at) in raw {
        let flow = match flows.entry(flow_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let f = flow_of(db.conn(), flow_id)?;
                v.insert(f.clone());
                f
            }
        };
        let log: Vec<WfLogEntry> = serde_json::from_str(&log_json).unwrap_or_default();
        let (flow_name, current_label) = match &flow {
            Some(f) => (
                f.name.clone(),
                f.nodes
                    .iter()
                    .find(|n| n.id == current_node)
                    .map(|n| node_label(n))
                    .unwrap_or_else(|| current_node.clone()),
            ),
            None => ("(流程已删除)".to_string(), current_node.clone()),
        };
        out.push(WfInstance {
            id,
            flow_id,
            flow_name,
            biz_type: biz_type.clone(),
            biz_label: biz_label(&biz_type).to_string(),
            biz_id,
            current_node,
            current_label,
            status,
            log,
            created_at,
        });
    }
    Ok(out)
}

fn node_label(n: &WfNode) -> String {
    if !n.name.trim().is_empty() {
        n.name.trim().to_string()
    } else {
        match n.node_type.as_str() {
            "start" => "开始".to_string(),
            "approve" => "审批".to_string(),
            "condition" => "条件".to_string(),
            _ => "消息".to_string(),
        }
    }
}

fn first_approve(flow: &WfFlow) -> Option<&WfNode> {
    flow.nodes.iter().find(|n| n.node_type == "approve")
}

fn normal_next(flow: &WfFlow, from: &str) -> Option<String> {
    flow.edges
        .iter()
        .find(|e| e.from_node == from && e.kind == "normal")
        .map(|e| e.to_node.clone())
}

fn reject_next(flow: &WfFlow, from: &str) -> Option<String> {
    if let Some(e) = flow
        .edges
        .iter()
        .find(|e| e.from_node == from && e.kind == "reject")
    {
        return Some(e.to_node.clone());
    }
    flow.nodes
        .iter()
        .find(|n| n.id == from)
        .map(|n| n.reject_to.clone())
        .filter(|s| !s.is_empty())
}

fn instance_of(conn: &rusqlite::Connection, biz_type: &str, biz_id: i64) -> DbResult<Option<(i64, String, String, String)>> {
    let row = conn
        .query_row(
            "SELECT id,status,current_node,log_json FROM workflow_instance
             WHERE biz_type=?1 AND biz_id=?2",
            rusqlite::params![biz_type, biz_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(row)
}

/// 审批拦截器（核心）：单据审批动作先过工作流。
/// - 无已发布流程 → `NoFlow`（调用方执行原有直接审批 = 默认流，向后兼容）
/// - 有流程且未到终态 → `Pending`（仅推进实例，调用方返回下一节点名，不执行业务动作）
/// - 到终态 → `Final`（调用方执行其原有业务审批/驳回）
/// 参与人规则：节点参与人为空 = 需要「审核权限」（VoucherAudit）；非空 = 命中参与人角色
/// 或持有审核权限（审核人/主管/管理员可兜底）。会签策略字段存档，v1 单人通过即过；
/// 条件连线字段存档，v1 按 normal 边顺序取第一条（分支选择后续迭代）。
pub fn intercept(
    db: &Db,
    biz_type: &str,
    biz_id: i64,
    user: &User,
    approve: bool,
    comment: &str,
) -> DbResult<Gate> {
    let flow = match published_flow_for(db, biz_type)? {
        Some(f) => f,
        None => return Ok(Gate::NoFlow),
    };
    let first = first_approve(&flow)
        .ok_or_else(|| fincore::FinError::state("流程未配置审批节点"))?
        .clone();
    let existing = instance_of(db.conn(), biz_type, biz_id)?;
    let tx = db.write_tx()?;
    let (inst_id, cur) = match existing {
        None => {
            tx.execute(
                "INSERT INTO workflow_instance(flow_id,biz_type,biz_id,current_node,status,log_json,created_at)
                 VALUES(?1,?2,?3,?4,'running','[]',?5)",
                rusqlite::params![flow.id, biz_type, biz_id, first.id, now()],
            )?;
            (tx.last_insert_rowid(), first.id.clone())
        }
        Some((id, ref status, ref cur, _)) if status == "running" => (id, cur.clone()),
        Some((id, ref status, _, _)) if status == "rejected" => {
            // 驳回后重新发起：重置回第一个审批节点
            let n = tx.execute(
                "UPDATE workflow_instance SET current_node=?2, status='running', log_json='[]'
                 WHERE id=?1 AND status='rejected'",
                rusqlite::params![id, first.id],
            )?;
            if n == 0 {
                return Err(fincore::FinError::state("实例状态已变化，请刷新重试").into());
            }
            (id, first.id.clone())
        }
        Some((_, _, _, _)) => return Ok(Gate::Final { approved: true }), // 已批准 → 终态幂等
    };
    let node = flow
        .nodes
        .iter()
        .find(|n| n.id == cur)
        .ok_or_else(|| fincore::FinError::state("流程当前节点不存在（流程可能被改）"))?
        .clone();
    // 审批权限：审核权限（审核人/主管/管理员）恒可批；否则必须命中节点参与人角色
    let audit_ok = user.can(Perm::VoucherAudit);
    let hit = !node.participants.is_empty()
        && user
            .all_roles()
            .iter()
            .any(|r| node.participants.contains(&role_code(r)));
    if !audit_ok && !hit {
        return Err(fincore::FinError::state(format!(
            "当前节点【{}】的审批人不含您{}",
            node_label(&node),
            if node.participants.is_empty() {
                "（该节点要求审核权限）".to_string()
            } else {
                format!("（参与人：{}）", node.participants.join("、"))
            }
        ))
        .into());
    }
    // 记轨迹（读-改-写 log_json）
    let log_s: String = tx.query_row(
        "SELECT log_json FROM workflow_instance WHERE id=?1",
        [inst_id],
        |r| r.get(0),
    )?;
    let mut log: Vec<WfLogEntry> = serde_json::from_str(&log_s).unwrap_or_default();
    log.push(WfLogEntry {
        node: node.id.clone(),
        action: if approve { "approve" } else { "reject" }.to_string(),
        who: user.username.clone(),
        at: now(),
    });
    tx.execute(
        "UPDATE workflow_instance SET log_json=?2 WHERE id=?1",
        rusqlite::params![inst_id, serde_json::to_string(&log)?],
    )?;
    let next = if approve {
        normal_next(&flow, &node.id)
    } else {
        reject_next(&flow, &node.id)
    };
    match next {
        None => {
            // 无出边 = 流程终点（approve）；驳回无路径 = 终态 rejected
            let status = if approve { "approved" } else { "rejected" };
            tx.execute(
                "UPDATE workflow_instance SET status=?2 WHERE id=?1",
                rusqlite::params![inst_id, status],
            )?;
            tx.commit()?;
            Ok(Gate::Final { approved: approve })
        }
        Some(nid) => {
            let label = flow
                .nodes
                .iter()
                .find(|n| n.id == nid)
                .map(node_label)
                .unwrap_or_else(|| nid.clone());
            let n = tx.execute(
                "UPDATE workflow_instance SET current_node=?2 WHERE id=?1 AND status='running'",
                rusqlite::params![inst_id, nid],
            )?;
            if n == 0 {
                return Err(fincore::FinError::state("实例状态已变化，请刷新重试").into());
            }
            tx.commit()?;
            Ok(Gate::Pending { next: label })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn input(id: i64, name: &str, nodes: Vec<WfNode>, edges: Vec<WfEdge>) -> WfFlowInput {
        WfFlowInput {
            id,
            name: name.to_string(),
            biz_type: BIZ_QUOTATION.to_string(),
            nodes,
            edges,
        }
    }
    fn n(id: &str, t: &str, name: &str, parts: Vec<&str>) -> WfNode {
        WfNode {
            id: id.to_string(),
            node_type: t.to_string(),
            name: name.to_string(),
            participants: parts.into_iter().map(String::from).collect(),
            strategy: "all".to_string(),
            reject_to: String::new(),
            x: 0.0,
            y: 0.0,
        }
    }
    fn e(id: &str, from: &str, to: &str, kind: &str) -> WfEdge {
        WfEdge {
            id: id.to_string(),
            from_node: from.to_string(),
            to_node: to.to_string(),
            kind: kind.to_string(),
            condition: String::new(),
        }
    }

    #[test]
    fn flow_save_validation_and_publish() {
        let db = mem();
        // 无 start → 拒
        assert!(flow_save(&db, &input(0, "f", vec![n("a1", "approve", "审批", vec![])], vec![]), "u").is_err());
        // start 不唯一 → 拒
        assert!(flow_save(
            &db,
            &input(0, "f", vec![n("s1", "start", "开始", vec![]), n("s2", "start", "开始", vec![])], vec![]),
            "u",
        )
        .is_err());
        // 连线端点不存在 → 拒
        assert!(flow_save(
            &db,
            &input(0, "f", vec![n("s1", "start", "开始", vec![])], vec![e("e1", "s1", "ghost", "normal")]),
            "u",
        )
        .is_err());
        // 正常保存 → 发布需要审批节点
        let ok = input(
            0,
            "报价审批流",
            vec![n("s1", "start", "开始", vec![]), n("a1", "approve", "主管审批", vec![])],
            vec![e("e1", "s1", "a1", "normal")],
        );
        let id = flow_save(&db, &ok, "u").unwrap();
        assert!(flow_set_status(&db, id, true, "u").is_ok());
        // 撤回 + 删除
        flow_set_status(&db, id, false, "u").unwrap();
        flow_delete(&db, id).unwrap();
        assert!(flow_list(&db).unwrap().is_empty());

        // 无审批节点发布 → 拒
        let no_approve = input(0, "g", vec![n("s1", "start", "开始", vec![])], vec![]);
        let id2 = flow_save(&db, &no_approve, "u").unwrap();
        assert!(flow_set_status(&db, id2, true, "u").is_err(), "无审批节点不能发布");
    }

    #[test]
    fn intercept_lifecycle() {
        let db = mem();
        let nodes = vec![
            n("s1", "start", "开始", vec![]),
            n("a1", "approve", "初审", vec![]),
            n("a2", "approve", "复核", vec![]),
        ];
        let edges = vec![e("e1", "s1", "a1", "normal"), e("e2", "a1", "a2", "normal")];
        let id = flow_save(&db, &input(0, "两节点流", nodes, edges), "u").unwrap();
        flow_set_status(&db, id, true, "u").unwrap();

        // 无审核权限且参与人不含他 → 拒
        let viewer = User::new("v", "只读", Role::Viewer);
        assert!(
            intercept(&db, BIZ_QUOTATION, 1, &viewer, true, "").is_err(),
            "无审批权应被拒"
        );
        // 主管（含审核权）推进
        let sup = User::new("s", "主管", Role::Supervisor);
        match intercept(&db, BIZ_QUOTATION, 1, &sup, true, "").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "复核"),
            other => panic!("第一次应推进到下一节点：{other:?}"),
        }
        match intercept(&db, BIZ_QUOTATION, 1, &sup, true, "同意").unwrap() {
            Gate::Final { approved } => assert!(approved, "第二节点为终点 → Final(approved)"),
            other => panic!("第二次应到终态：{other:?}"),
        }
        // 终态幂等
        assert!(matches!(
            intercept(&db, BIZ_QUOTATION, 1, &sup, true, "").unwrap(),
            Gate::Final { approved: true }
        ));

        // 无已发布流程的类型 → NoFlow（默认流）
        assert!(matches!(
            intercept(&db, BIZ_CLAIM, 9, &sup, true, "").unwrap(),
            Gate::NoFlow
        ));
    }

    #[test]
    fn intercept_reject_and_restart() {
        let db = mem();
        let nodes = vec![
            n("s1", "start", "开始", vec![]),
            n("a1", "approve", "初审", vec![]),
            n("a2", "approve", "复核", vec![]),
        ];
        let edges = vec![e("e1", "s1", "a1", "normal"), e("e2", "a1", "a2", "normal")];
        let id = flow_save(&db, &input(0, "驳回重审流", nodes, edges), "u").unwrap();
        flow_set_status(&db, id, true, "u").unwrap();

        let sup = User::new("s", "主管", Role::Supervisor);
        // 首个动作即驳回（无 reject 路径）→ 终态 rejected
        assert!(matches!(
            intercept(&db, BIZ_QUOTATION, 7, &sup, false, "不同意").unwrap(),
            Gate::Final { approved: false }
        ));
        // 再次发起 → 实例重置回初审节点，正常推进
        match intercept(&db, BIZ_QUOTATION, 7, &sup, true, "").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "复核"),
            other => panic!("驳回后重新发起应从头推进：{other:?}"),
        }
    }
}

