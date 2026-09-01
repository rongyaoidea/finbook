# FinBook 安全加固待办事项

## 已完成（20项）

### P0 级安全修复 ✅
- CRIT-1: 默认监听改为 127.0.0.1
- HIGH-1: Web层落地DataScope
- HIGH-2: voucher_unaudit权限校验+语义修正
- HIGH-3: 凭证删除守卫
- HIGH-4: prepared_by统一为username
- HIGH-5: DataScope默认收紧
- HIGH-6: must_change_pwd服务端拦截
- HIGH-7: Cookie Secure标志

### P1 级修复 ✅
- H-C-1: 资产负债表.neg()笔误
- H-2: 核销金额精度修复
- M-1: 登录枚举消除
- M-3: 锁定窗口单位修复
- M-5: 附件大小上限+白名单
- M-6: 附件路径穿越防护
- M-9: 未认证接口路径泄露
- M-10: parse_money Unicode支持
- M-14: argon2降级防护

## 待后续迭代项

### 中优先级（建议下一版本处理）

| ID | 问题 | 建议方案 | 预估工作量 |
|----|------|----------|------------|
| M-2 | 登录失败IP限流 | Web层按remote_addr计数，固定窗口限流 | 小 |
| M-4 | 设备绑定强化 | 首次绑定后禁止客户端修改，服务端生成device_hash | 中 |
| M-7 | 导入事务化 | import_begin_rows/import_vouchers_rows包unchecked_transaction | 中 |

### 需产品决策

| ID | 问题 | 选项 |
|----|------|------|
| H-3 | 余额是否过滤未记账凭证 | A: 只算Posted B: 算Posted+Audited C: 当前行为 |
| M-15 | 借贷平衡判定精度 | A: round2容忍 B: 全精度严格 C: 可选配置 |

### 低优先级（可渐进优化）

| ID | 问题 | 说明 |
|----|------|------|
| L-1 | Money除零返回0 | 改Result或checked_div |
| L-2 | Period::from_ymm异常值 | 加校验返回Result |
| L-3~L-14 | 各种边界/死代码/风格 | 见原审查报告 |

## 推荐修复顺序

```
1. M-2 IP限流（安全加固，工作量大但收益明确）
2. M-7 导入事务化（数据完整性关键）
3. H-3/M-15 产品决策
4. M-4 设备绑定强化
5. L系列渐进优化
```

## 当前分支状态

- HEAD: `22a6144 docs: 更新安全加固修复报告（20项已修复）`
- 远程: origin/main 已同步
- 测试: fincore 87, findb 100, finweb 11 全部通过
