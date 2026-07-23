# 布局重构实施计划

## 目标
从 4 宫格布局（files/sysmon/agents/shells）改为 3 分区布局（left + right-top + right-bottom）

## 核心变更

### 1. 布局结构
**当前（4宫格）**：
```
┌─ files ──┬─ agents ─┐
│          │          │
├─ sysmon ─┼─ shells ─┤
│          │          │
└──────────┴──────────┘
```
- 左列分上下：files (65%) / sysmon (35%)
- 右列分上下：agents (55%) / shells (45%)

**目标（3分区）**：
```
┌─ left ───┬─ agents ─┐
│          │          │
│  (yazi/  │          │
│  viewer/ ├─ shells ─┤
│  gitui)  │ (bottom+ │
│          │  shells) │
└──────────┴──────────┘
```
- **左列（35%）**：单 pane，yazi/viewer/gitui 模式切换，**占满全高**
- **右上（55%）**：agents tab 组
- **右下（45%）**：shells tab 组（bottom 是第一个 tab）

**关键约束**：左 pane 下边界和右下 pane 下边界对齐（左列占满全高）

### 2. 需要修改的文件

#### 2.1 核心常量定义
**文件**: `crates/rimeterm-core/src/tabs.rs`
- **删除**: `BUILTIN_SYSMON` 常量（line 27）
- **保留**: `BUILTIN_FILES`, `BUILTIN_AGENTS`, `BUILTIN_SHELLS`

#### 2.2 布局构建
**文件**: `crates/rimeterm-tui/src/app.rs`

**位置**: lines 710-728（layout construction）

**当前代码**：
```rust
let root = LayoutNode::split(
    Direction::Horizontal,
    vec![0.35, 0.65],
    vec![
        LayoutNode::split(
            Direction::Vertical,
            vec![0.65, 0.35],
            vec![LayoutNode::tabs(files), LayoutNode::tabs(sysmon)],
        ),
        LayoutNode::split(
            Direction::Vertical,
            vec![0.55, 0.45],
            vec![LayoutNode::tabs(agents), LayoutNode::tabs(shells)],
        ),
    ],
);
```

**目标代码**：
```rust
let root = LayoutNode::split(
    Direction::Horizontal,
    vec![0.35, 0.65],
    vec![
        LayoutNode::tabs(files),  // 左列：单 pane，占满全高
        LayoutNode::split(
            Direction::Vertical,
            vec![0.55, 0.45],
            vec![LayoutNode::tabs(agents), LayoutNode::tabs(shells)],
        ),
    ],
);
```

#### 2.3 Sysmon 组移除
**文件**: `crates/rimeterm-tui/src/app.rs`

**位置**: lines 530-560（sysmon members 构建）、lines 691-696（sysmon TabGroup 创建）

**操作**：
1. 删除 `sysmon_members` 构建逻辑
2. 删除 `sysmon` TabGroup 创建
3. **将 bottom 移入 shells 组作为第一个 tab**

**目标代码**（shells 组修改）：
```rust
// shells 组：bottom 是第一个 tab，后续是 shell tabs
let mut shells_members = Vec::new();

// 1. 先加 bottom（如果配置中有）
if let Some(bottom_spec) = config.sysmon.tools.iter().find(|t| t.kind == "bottom") {
    let bottom_pane = build_external_pane(
        &mut panes,
        bottom_spec,
        BUILTIN_SHELLS,
        80,
        24,
        &workspace_root,
        redraw_tx.clone(),
        osc_tx.clone(),
    )?;
    shells_members.push(bottom_pane.id());
}

// 2. 再加第一个 shell
let first = spawn_shell(...);
shells_members.push(first_id);

let shells = TabGroup::new(
    BUILTIN_SHELLS,
    shells_members,
    MembersPolicy::Open { max: 16 },
    PaneKind::Shell,  // bottom 和 shell 都算 Shell 类
);
```

#### 2.4 焦点导航
**文件**: `crates/rimeterm-tui/src/app.rs`

**函数**: `neighbor_group` (lines 3315-3334)

**当前逻辑**（4宫格）：
```rust
// 1=left, 2=right, 3=up, 4=down
match (dir, same) {
    (1, AGENTS) => FILES,
    (1, SHELLS) => SYSMON,
    (2, FILES) => AGENTS,
    (2, SYSMON) => SHELLS,
    (3, SYSMON) => FILES,
    (3, SHELLS) => AGENTS,
    (4, FILES) => SYSMON,
    (4, AGENTS) => SHELLS,
    _ => same,
}
```

**目标逻辑**（3分区）：
```rust
// 1=left, 2=right, 3=up, 4=down
match (dir, same) {
    // 左右切换
    (1, AGENTS) => FILES,
    (1, SHELLS) => FILES,
    (2, FILES) => AGENTS,
    
    // 上下切换（仅右列）
    (3, SHELLS) => AGENTS,
    (4, AGENTS) => SHELLS,
    
    // 左列无上下
    (3 | 4, FILES) => same,
    
    _ => same,
}
```

#### 2.5 Quadrant 命令
**文件**: `crates/rimeterm-tui/src/app.rs`

**位置**: lines 2300-2306（quadrant mapping）

**当前**：
```rust
let gid = match quad {
    1 => BUILTIN_FILES,
    2 => BUILTIN_AGENTS,
    3 => BUILTIN_SYSMON,
    4 => BUILTIN_SHELLS,
    _ => return,
};
```

**目标**：
```rust
let gid = match quad {
    1 => BUILTIN_FILES,
    2 => BUILTIN_AGENTS,
    3 => BUILTIN_SHELLS,
    _ => return,
};
```

#### 2.6 Resize 目标
**文件**: `crates/rimeterm-tui/src/app.rs`

**函数**: `target_divider` (lines 350-380)

**操作**：删除所有 `BUILTIN_SYSMON` 相关分支

#### 2.7 Layout 路径
**文件**: `crates/rimeterm-tui/src/app.rs`

**函数**: `paths_for_group` (lines 386-394)

**当前**：
```rust
let column = match gid {
    g if g == BUILTIN_FILES || g == BUILTIN_SYSMON => 0,
    g if g == BUILTIN_AGENTS || g == BUILTIN_SHELLS => 1,
    _ => return Vec::new(),
};
```

**目标**：
```rust
let column = match gid {
    g if g == BUILTIN_FILES => 0,
    g if g == BUILTIN_AGENTS || g == BUILTIN_SHELLS => 1,
    _ => return Vec::new(),
};
```

#### 2.8 Group 名称解析
**文件**: `crates/rimeterm-tui/src/app.rs`

**多处出现**：
- `parse_layout_reset_args` (line 4267)
- `parse_str_group_id` (line 4652)
- Error messages (line 178)

**操作**：删除 `"sysmon"` 映射

#### 2.9 Tests
**文件**: `crates/rimeterm-tui/src/app.rs`

**位置**: lines 4904-4930（neighbor_group tests）

**操作**：删除所有涉及 `BUILTIN_SYSMON` 的测试用例

#### 2.10 Config
**文件**: `crates/rimeterm-config/src/lib.rs`

**操作**：
- 保留 `SysmonConfig` 结构（bottom 配置仍需要）
- 更新注释说明 bottom 现在是 shells 组的第一个 tab

### 3. 实施步骤

1. **准备阶段**
   - [x] 读取并理解现有代码结构
   - [x] 制定详细实施计划
   - [ ] 运行现有测试确保基线

2. **核心修改**
   - [ ] 修改 `tabs.rs`：注释掉 `BUILTIN_SYSMON`（保留以防回滚）
   - [ ] 修改 `app.rs` layout construction：改为 3 分区
   - [ ] 修改 shells 组：将 bottom 作为第一个 tab
   - [ ] 删除 sysmon 组构建逻辑

3. **导航修改**
   - [ ] 更新 `neighbor_group` 函数
   - [ ] 更新 quadrant 命令映射（Alt+1/2/3）
   - [ ] 更新 resize 相关函数

4. **清理**
   - [ ] 删除所有 sysmon 相关的测试
   - [ ] 更新错误消息
   - [ ] 更新注释和文档字符串

5. **验证**
   - [ ] 编译通过
   - [ ] 运行测试套件
   - [ ] 手动测试：启动、焦点导航、resize、tab 切换

## 验收标准

1. ✅ 左列占满全高（与右下 pane 下边界对齐）
2. ✅ 左列只有 files 组（未来支持 yazi/viewer/gitui 切换）
3. ✅ 右上是 agents 组
4. ✅ 右下是 shells 组，bottom 是第一个 tab
5. ✅ Alt+1/2/3 分别跳转到 left/agents/shells
6. ✅ Alt+H/L 左右列切换
7. ✅ Alt+K/J 右列上下切换，左列无效
8. ✅ 所有测试通过
9. ✅ 无 trippy 引用
10. ✅ 无 sysmon 组引用

## 回滚计划

如果实施过程中遇到重大问题：
1. `git stash` 保存当前进度
2. 恢复到修改前的 commit
3. Review 失败原因
4. 调整计划后重新实施
