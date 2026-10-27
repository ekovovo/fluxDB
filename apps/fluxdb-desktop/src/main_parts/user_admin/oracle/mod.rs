// Oracle 用户与权限页，保留既有 include! 作用域。
//
// 结构对齐 SQL Server 面板，但数据模型是 Oracle 专属：账号即 schema，没有库/登录两层；
// 授权明细只能读当前账号自己，继承来的授权只读不可撤销（docs/oracle.md §8）。
include!("tasks.rs");
include!("subscriptions.rs");
include!("layout.rs");
include!("accounts.rs");
include!("grants.rs");
include!("form.rs");
include!("preview.rs");
include!("shared.rs");
