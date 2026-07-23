# 鼠标冲突修复说明

## 问题描述

在支持鼠标选择和复制粘贴后，出现了两个冲突：

1. **右键菜单与复制粘贴冲突**
   - 当用户选中文本后，右键点击应该触发复制（类似 GNOME Terminal、KDE Konsole 的行为）
   - 但原实现中右键总是打开上下文菜单，导致无法通过右键复制选中的文本

2. **鼠标选择与 yazi 分隔符拖动冲突**
   - yazi 内部绘制了三列布局的分隔符（用字符渲染）
   - 用户点击 yazi 内部的分隔符想拖动时，rimeterm 会优先判断是否点击在自己的 layout divider 上
   - 导致用户无法拖动 yazi 的内部分隔符

## 解决方案

### 1. 右键菜单冲突修复

**核心思路**：在打开上下文菜单前，先检查目标 pane 是否有活动的文本选择。如果有，将右键事件转发给 pane 进行复制，而不是打开菜单。

**修改文件**：

#### `crates/rimeterm-core/src/pane.rs`
- 在 `PaneProvider` trait 中添加 `has_active_selection()` 方法
- 默认返回 `false`，只有 `PtyPane` 会重写此方法

```rust
fn has_active_selection(&self) -> bool {
    false
}
```

#### `crates/rimeterm-tui/src/pty_pane.rs`
- 在 `impl PaneProvider for PtyPane` 中实现 `has_active_selection()`
- 添加右键点击时的复制处理逻辑

```rust
fn has_active_selection(&self) -> bool {
    self.selection.is_active()
}

// 在 on_mouse 中添加：
MouseEventKind::Down(MouseButton::Right) => {
    if self.selection.is_active() {
        self.copy_selection();
        self.selection.clear();
        return true;
    }
    false
}
```

#### `crates/rimeterm-tui/src/app.rs`
- 修改右键处理逻辑：先检查 pane 是否有选择，有则转发，无则打开菜单

```rust
if let MouseEventKind::Down(MouseButton::Right) = m.kind {
    // 检查点击位置的 pane 是否有活动选择
    if let Some((pane_id, outer_rect)) = self.pane_outer_at(m.column, m.row) {
        if let Some(pane) = self.panes.get_mut(pane_id) {
            if pane.has_active_selection() {
                let _ = pane.on_mouse(m, outer_rect);
                return;
            }
        }
    }
    // 没有选择 — 打开上下文菜单
    self.open_context_menu(m.column, m.row);
    return;
}
```

### 2. yazi 分隔符拖动冲突修复

**核心思路**：当子程序（如 yazi）请求了鼠标控制时，优先将鼠标事件转发给子程序，而不是先检查 rimeterm 的 divider 拖动。

**修改文件**：

#### `crates/rimeterm-core/src/pane.rs`
- 在 `PaneProvider` trait 中添加 `wants_mouse_priority()` 方法
- 用于查询 pane 是否需要优先接收鼠标事件

```rust
fn wants_mouse_priority(&self, shift_held: bool) -> bool {
    let _ = shift_held;
    false
}
```

#### `crates/rimeterm-tui/src/pty_pane.rs`
- 实现 `wants_mouse_priority()` 方法
- 当子程序请求了 xterm 鼠标跟踪且用户未按住 Shift 时返回 `true`

```rust
fn wants_mouse_priority(&self, shift_held: bool) -> bool {
    self.child_wants_mouse() && !shift_held
}
```

#### `crates/rimeterm-tui/src/app.rs`
- 修改左键 Down 事件的路由优先级
- 先检查 pane 是否需要优先处理鼠标，如果需要则转发
- 否则才检查 divider 拖动、tab strip 点击等 rimeterm 自己的交互

```rust
if let MouseEventKind::Down(MouseButton::Left) = m.kind {
    // 1. 检查 pane 是否需要鼠标优先权
    if let Some((pane_id, outer_rect)) = self.pane_outer_at(m.column, m.row) {
        if let Some(pane) = self.panes.get(pane_id) {
            if pane.wants_mouse_priority(m.modifiers.contains(KeyModifiers::SHIFT)) {
                // 转发给子程序处理
                let _ = pane;
                if let Some(pane_mut) = self.panes.get_mut(pane_id) {
                    let _ = pane_mut.on_mouse(m, outer_rect);
                }
                return;
            }
        }
    }

    // 2. 没有子程序优先权 — 检查 rimeterm 自己的交互区域
    // 2.1 Divider drag
    if let Some(d) = self.last_dividers.iter()
        .find(|d| point_in_rect(m.column, m.row, d.visual.rect))
        .cloned()
    {
        self.start_divider_drag(d, m.column, m.row);
        return;
    }
    // 2.2 Tab strip
    // 2.3 Pane focus
    // ...
}
```

## 用户体验改进

### 1. 右键复制功能
- **有文本选择时**：右键点击 → 复制选中内容到剪贴板 + 清除选择高亮
- **无文本选择时**：右键点击 → 打开上下文菜单（原有行为）
- 符合 Linux 终端的常见交互习惯（GNOME Terminal、Konsole）

### 2. yazi 内部交互
- yazi 请求鼠标控制时，点击 yazi 内部的分隔符会被转发给 yazi
- yazi 可以正常响应自己的三列布局拖动
- 按住 Shift 仍可强制本地选择（原有设计保留）

### 3. 兼容性
- 不影响其他 TUI 应用（vim、htop、less 等）的鼠标交互
- bash/pwsh/fish 等 shell 提示符下仍然可以正常文本选择
- rimeterm 的 layout divider 拖动功能仍然正常工作（在非 TUI 应用区域）

## 测试建议

1. **右键复制测试**：
   - 在 shell 中选择一段文本，右键点击 → 验证文本被复制到剪贴板
   - 在没有选择时右键点击 → 验证上下文菜单正常打开

2. **yazi 分隔符测试**：
   - 打开 yazi，尝试拖动左右两个竖线分隔符 → 验证 yazi 列宽可以调整
   - 按住 Shift 后在 yazi 中拖动鼠标 → 验证文本选择功能正常

3. **边界情况测试**：
   - 在 vim 中测试鼠标点击和选择 → 验证 vim 的鼠标功能正常
   - 在 rimeterm 的 layout divider 上拖动 → 验证窗格大小调整正常
   - 在 tab strip 上点击 → 验证 tab 切换和关闭正常

## 技术细节

### 事件路由优先级（修改后）

#### 右键事件：
1. 检查目标 pane 是否有活动选择
2. 有选择 → 转发给 pane 处理复制
3. 无选择 → 打开上下文菜单

#### 左键 Down 事件：
1. 检查是否有活动的 divider 拖动（拖动中途的事件）
2. 检查目标 pane 是否需要鼠标优先权（子程序请求了鼠标控制）
3. 有优先权 → 转发给 pane
4. 无优先权 → 检查 rimeterm 的交互区域：
   - Divider drag 启动
   - Tab strip 点击
   - Pane focus + 转发事件

### 关键设计决策

1. **使用 trait 方法而非类型转换**：避免使用 `unsafe` 的类型转换，通过在 `PaneProvider` trait 中添加方法来查询状态

2. **保留 Shift 强制本地控制**：即使子程序请求了鼠标，按住 Shift 仍可强制本地文本选择（原有设计）

3. **右键清除选择**：复制后自动清除高亮，避免用户困惑（已复制还是未复制）

4. **优先级明确**：每个事件处理函数都有清晰的优先级注释，便于维护和调试
