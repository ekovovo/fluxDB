:On Error exit
-- 仅首次准备时整份执行；后续测试不要重跑。
-- 遇到同名库/用户/表即停止，不删除或覆盖现有对象。
-- 所有测试数据一次准备完成，执行方式见同目录《测试教程.md》。

USE master;
GO
IF SUSER_ID(N'completion_tester') IS NOT NULL
    THROW 51000, 'completion_tester already exists; refusing to modify it.', 1;
IF DB_ID(N'completion_lab') IS NOT NULL
    THROW 51000, 'completion_lab already exists; refusing to modify it.', 1;
GO
CREATE LOGIN completion_tester WITH PASSWORD=N'o9i8u7y6', CHECK_POLICY=OFF, CHECK_EXPIRATION=OFF;
GO
CREATE DATABASE completion_lab;
GO
USE completion_lab;
GO
SET XACT_ABORT ON;
BEGIN TRANSACTION;
GO
CREATE SCHEMA lab;
GO
CREATE SCHEMA archive;
GO

CREATE TABLE lab.tenants (
    tenant_id INT PRIMARY KEY,
    tenant_name NVARCHAR(80) NOT NULL
);

CREATE TABLE lab.customers (
    tenant_id INT NOT NULL,
    customer_id INT NOT NULL,
    customer_name NVARCHAR(100) NOT NULL,
    email NVARCHAR(160),
    region NVARCHAR(30),
    credit_limit DECIMAL(12,2),
    created_at DATETIME2 NOT NULL,
    CONSTRAINT pk_customers PRIMARY KEY (tenant_id, customer_id),
    CONSTRAINT fk_customer_tenant FOREIGN KEY (tenant_id) REFERENCES lab.tenants(tenant_id)
);

CREATE TABLE lab.departments (
    department_id INT PRIMARY KEY,
    department_name NVARCHAR(80) NOT NULL
);

CREATE TABLE lab.employees (
    employee_id INT PRIMARY KEY,
    employee_name NVARCHAR(100) NOT NULL,
    department_id INT,
    manager_id INT,
    salary DECIMAL(12,2),
    CONSTRAINT fk_employee_department FOREIGN KEY (department_id) REFERENCES lab.departments(department_id),
    CONSTRAINT fk_employee_manager FOREIGN KEY (manager_id) REFERENCES lab.employees(employee_id)
);

CREATE TABLE lab.categories (
    category_id INT PRIMARY KEY,
    parent_id INT,
    category_name NVARCHAR(80) NOT NULL,
    CONSTRAINT fk_category_parent FOREIGN KEY (parent_id) REFERENCES lab.categories(category_id)
);

CREATE TABLE lab.products (
    product_id INT PRIMARY KEY,
    category_id INT,
    product_name NVARCHAR(100) NOT NULL,
    unit_price DECIMAL(12,2) NOT NULL,
    sku NVARCHAR(50) NOT NULL UNIQUE,
    CONSTRAINT fk_product_category FOREIGN KEY (category_id) REFERENCES lab.categories(category_id)
);

CREATE TABLE lab.orders (
    tenant_id INT NOT NULL,
    order_id INT NOT NULL,
    customer_id INT,
    sales_rep_id INT,
    status NVARCHAR(20) NOT NULL,
    total_amount DECIMAL(12,2) NOT NULL,
    ordered_at DATETIME2 NOT NULL,
    note NVARCHAR(200),
    CONSTRAINT pk_orders PRIMARY KEY (tenant_id, order_id),
    CONSTRAINT fk_order_customer FOREIGN KEY (tenant_id, customer_id) REFERENCES lab.customers(tenant_id, customer_id),
    CONSTRAINT fk_order_tenant FOREIGN KEY (tenant_id) REFERENCES lab.tenants(tenant_id),
    CONSTRAINT fk_order_employee FOREIGN KEY (sales_rep_id) REFERENCES lab.employees(employee_id),
    CONSTRAINT ck_order_status CHECK (status IN ('new','paid','cancelled'))
);

CREATE TABLE lab.order_items (
    tenant_id INT NOT NULL,
    order_id INT NOT NULL,
    line_no INT NOT NULL,
    product_id INT NOT NULL,
    quantity INT NOT NULL,
    unit_price DECIMAL(12,2) NOT NULL,
    discount DECIMAL(12,2) NOT NULL,
    CONSTRAINT pk_order_items PRIMARY KEY (tenant_id, order_id, line_no),
    CONSTRAINT fk_item_order FOREIGN KEY (tenant_id, order_id) REFERENCES lab.orders(tenant_id, order_id),
    CONSTRAINT fk_item_product FOREIGN KEY (product_id) REFERENCES lab.products(product_id),
    CONSTRAINT ck_item_quantity CHECK (quantity > 0)
);

CREATE TABLE lab.payments (
    payment_id INT PRIMARY KEY,
    tenant_id INT NOT NULL,
    order_id INT NOT NULL,
    amount DECIMAL(12,2) NOT NULL,
    paid_at DATETIME2,
    method NVARCHAR(20),
    CONSTRAINT fk_payment_order FOREIGN KEY (tenant_id, order_id) REFERENCES lab.orders(tenant_id, order_id)
);

CREATE TABLE lab.audit_events (
    event_id INT PRIMARY KEY,
    entity_type NVARCHAR(30) NOT NULL,
    entity_id INT,
    payload NVARCHAR(MAX),
    created_at DATETIME2 NOT NULL
);

CREATE TABLE lab.[Order Details] (
    [Row ID] INT PRIMARY KEY,
    [select] NVARCHAR(80),
    [MixedCase] NVARCHAR(80),
    [客户名称] NVARCHAR(80),
    [unit-price] DECIMAL(12,2)
);

CREATE TABLE lab.[quote`bracket]]double"] (
    [odd`col]]name"] NVARCHAR(80)
);

CREATE TABLE lab.wide_metrics (
    metric_id INT PRIMARY KEY,
    metric_01 DECIMAL(12,2),
    metric_02 DECIMAL(12,2),
    metric_03 DECIMAL(12,2),
    metric_04 DECIMAL(12,2),
    metric_05 DECIMAL(12,2),
    metric_06 DECIMAL(12,2),
    metric_07 DECIMAL(12,2),
    metric_08 DECIMAL(12,2),
    metric_09 DECIMAL(12,2),
    metric_10 DECIMAL(12,2),
    metric_11 DECIMAL(12,2),
    metric_12 DECIMAL(12,2),
    metric_13 DECIMAL(12,2),
    metric_14 DECIMAL(12,2),
    metric_15 DECIMAL(12,2),
    metric_16 DECIMAL(12,2),
    metric_17 DECIMAL(12,2),
    metric_18 DECIMAL(12,2),
    metric_19 DECIMAL(12,2),
    metric_20 DECIMAL(12,2),
    metric_21 DECIMAL(12,2),
    metric_22 DECIMAL(12,2),
    metric_23 DECIMAL(12,2),
    metric_24 DECIMAL(12,2),
    metric_25 DECIMAL(12,2),
    metric_26 DECIMAL(12,2),
    metric_27 DECIMAL(12,2),
    metric_28 DECIMAL(12,2),
    metric_29 DECIMAL(12,2),
    metric_30 DECIMAL(12,2),
    metric_31 DECIMAL(12,2),
    metric_32 DECIMAL(12,2),
    metric_33 DECIMAL(12,2),
    metric_34 DECIMAL(12,2),
    metric_35 DECIMAL(12,2),
    metric_36 DECIMAL(12,2),
    metric_37 DECIMAL(12,2),
    metric_38 DECIMAL(12,2),
    metric_39 DECIMAL(12,2),
    metric_40 DECIMAL(12,2),
    metric_41 DECIMAL(12,2),
    metric_42 DECIMAL(12,2),
    metric_43 DECIMAL(12,2),
    metric_44 DECIMAL(12,2),
    metric_45 DECIMAL(12,2),
    metric_46 DECIMAL(12,2),
    metric_47 DECIMAL(12,2),
    metric_48 DECIMAL(12,2)
);

CREATE TABLE archive.customers (tenant_id INT NOT NULL, customer_id INT NOT NULL, archived_name NVARCHAR(100), archive_reason NVARCHAR(100), PRIMARY KEY (tenant_id, customer_id));

CREATE INDEX ix_orders_customer ON lab.orders(tenant_id, customer_id);

INSERT INTO lab.tenants (tenant_id, tenant_name) VALUES (1, N'North Lab');

INSERT INTO lab.tenants (tenant_id, tenant_name) VALUES (2, N'South Lab');

INSERT INTO lab.customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (1, 101, N'Alice Chen', N'alice@example.invalid', N'east', 10000, N'2026-10-01 10:00:00');

INSERT INTO lab.customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (1, 102, N'Bob Li', NULL, N'west', 0, N'2026-10-02 10:00:00');

INSERT INTO lab.customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (1, 103, N'张三', N'zhang@example.invalid', N'east', NULL, N'2026-10-03 10:00:00');

INSERT INTO lab.customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (2, 101, N'Alice South', N'south@example.invalid', N'south', 5000, N'2026-10-04 10:00:00');

INSERT INTO lab.customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (2, 104, N'O''Neil', NULL, NULL, 500, N'2026-10-05 10:00:00');

INSERT INTO lab.departments (department_id, department_name) VALUES (10, N'Sales');

INSERT INTO lab.departments (department_id, department_name) VALUES (20, N'Engineering');

INSERT INTO lab.departments (department_id, department_name) VALUES (30, N'Empty Department');

INSERT INTO lab.employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (1, N'Director', 10, NULL, 30000);

INSERT INTO lab.employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (2, N'Sales A', 10, 1, 10000);

INSERT INTO lab.employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (3, N'Sales B', 10, 1, 10000);

INSERT INTO lab.employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (4, N'Lead', 20, 1, 20000);

INSERT INTO lab.employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (5, N'Engineer', 20, 4, 15000);

INSERT INTO lab.employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (6, N'Unassigned', NULL, NULL, NULL);

INSERT INTO lab.categories (category_id, parent_id, category_name) VALUES (1, NULL, N'All');

INSERT INTO lab.categories (category_id, parent_id, category_name) VALUES (2, 1, N'Hardware');

INSERT INTO lab.categories (category_id, parent_id, category_name) VALUES (3, 1, N'Software');

INSERT INTO lab.categories (category_id, parent_id, category_name) VALUES (4, 2, N'Accessories');

INSERT INTO lab.products (product_id, category_id, product_name, unit_price, sku) VALUES (11, 2, N'Keyboard', 100, N'KB-001');

INSERT INTO lab.products (product_id, category_id, product_name, unit_price, sku) VALUES (12, 4, N'Cable', 20, N'CB-001');

INSERT INTO lab.products (product_id, category_id, product_name, unit_price, sku) VALUES (13, 3, N'License', 300, N'SW-001');

INSERT INTO lab.products (product_id, category_id, product_name, unit_price, sku) VALUES (14, 2, N'Mouse', 50, N'MS-001');

INSERT INTO lab.products (product_id, category_id, product_name, unit_price, sku) VALUES (15, NULL, N'Unused Product', 0, N'UNUSED');

INSERT INTO lab.orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1001, 101, 2, N'paid', 140, N'2026-10-10 10:00:00', N'two lines');

INSERT INTO lab.orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1002, 101, 2, N'new', 300, N'2026-10-11 10:00:00', NULL);

INSERT INTO lab.orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1003, 102, 3, N'cancelled', 50, N'2026-10-12 10:00:00', N'cancelled');

INSERT INTO lab.orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1004, NULL, NULL, N'new', 0, N'2026-10-13 10:00:00', N'guest, no lines');

INSERT INTO lab.orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (2, 1001, 101, 3, N'paid', 120, N'2026-10-14 10:00:00', N'same order_id, different tenant');

INSERT INTO lab.orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (2, 1005, 104, NULL, N'paid', 300, N'2026-10-15 10:00:00', N'split payment');

INSERT INTO lab.orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (2, 1006, 104, 2, N'new', 0, N'2026-10-16 10:00:00', N'zero quantity not allowed; no items');

INSERT INTO lab.order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1001, 1, 11, 1, 100, 0);

INSERT INTO lab.order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1001, 2, 12, 2, 20, 0);

INSERT INTO lab.order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1002, 1, 13, 1, 300, 0);

INSERT INTO lab.order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1003, 1, 14, 1, 50, 0);

INSERT INTO lab.order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (2, 1001, 1, 11, 1, 100, 0);

INSERT INTO lab.order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (2, 1001, 2, 12, 1, 20, 0);

INSERT INTO lab.order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (2, 1005, 1, 13, 1, 300, 0);

INSERT INTO lab.payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (1, 1, 1001, 140, N'2026-10-10 10:00:00', N'card');

INSERT INTO lab.payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (2, 2, 1001, 120, N'2026-10-14 10:00:00', N'cash');

INSERT INTO lab.payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (3, 2, 1005, 100, N'2026-10-15 10:00:00', N'card');

INSERT INTO lab.payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (4, 2, 1005, 200, N'2026-10-16 10:00:00', N'bank');

INSERT INTO lab.audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (1, N'order', 1001, N'{"source":"web","tags":["vip","new"]}', N'2026-10-10 10:00:00');

INSERT INTO lab.audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (2, N'order', 1002, N'{"source":"mobile","score":0}', N'2026-10-11 10:00:00');

INSERT INTO lab.audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (3, N'customer', 103, NULL, N'2026-10-12 10:00:00');

INSERT INTO lab.audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (4, N'order', 1005, N'{"source":null,"nested":{"ok":true}}', N'2026-10-15 10:00:00');

INSERT INTO lab.[Order Details] ([Row ID], [select], [MixedCase], [客户名称], [unit-price]) VALUES (1, N'keyword column', N'Upper', N'张三', 12.5);

INSERT INTO lab.[Order Details] ([Row ID], [select], [MixedCase], [客户名称], [unit-price]) VALUES (2, N'second', NULL, N'李四', 0);

INSERT INTO lab.[quote`bracket]]double"] ([odd`col]]name"]) VALUES (N'escaped identifier');

INSERT INTO lab.wide_metrics (metric_id, metric_01, metric_02, metric_03, metric_04, metric_05, metric_06, metric_07, metric_08, metric_09, metric_10, metric_11, metric_12, metric_13, metric_14, metric_15, metric_16, metric_17, metric_18, metric_19, metric_20, metric_21, metric_22, metric_23, metric_24, metric_25, metric_26, metric_27, metric_28, metric_29, metric_30, metric_31, metric_32, metric_33, metric_34, metric_35, metric_36, metric_37, metric_38, metric_39, metric_40, metric_41, metric_42, metric_43, metric_44, metric_45, metric_46, metric_47, metric_48) VALUES (1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48);

INSERT INTO archive.customers (tenant_id, customer_id, archived_name, archive_reason) VALUES (1, 101, N'Old Alice', N'merged');

INSERT INTO archive.customers (tenant_id, customer_id, archived_name, archive_reason) VALUES (9, 999, N'Legacy', N'retired');

GO

CREATE VIEW lab.v_order_summary AS
SELECT o.tenant_id, o.order_id, c.customer_name, o.status, o.total_amount,
       COUNT(i.line_no) AS line_count
FROM lab.orders o
LEFT JOIN lab.customers c ON c.tenant_id=o.tenant_id AND c.customer_id=o.customer_id
LEFT JOIN lab.order_items i ON i.tenant_id=o.tenant_id AND i.order_id=o.order_id
GROUP BY o.tenant_id, o.order_id, c.customer_name, o.status, o.total_amount;

GO

CREATE VIEW lab.v_customer_totals AS
SELECT c.tenant_id, c.customer_id, c.customer_name, COUNT(o.order_id) AS order_count,
       COALESCE(SUM(o.total_amount),0) AS spent
FROM lab.customers c
LEFT JOIN lab.orders o ON o.tenant_id=c.tenant_id AND o.customer_id=c.customer_id
GROUP BY c.tenant_id,c.customer_id,c.customer_name;

GO
CREATE FUNCTION lab.lab_discount(@p_amount DECIMAL(12,2)) RETURNS DECIMAL(12,2) AS
BEGIN RETURN @p_amount * 0.9; END;
GO
CREATE PROCEDURE lab.lab_touch @p_id INT AS
BEGIN SET NOCOUNT ON; SELECT @p_id AS touched_id; END;
GO
EXEC sys.sp_addextendedproperty @name=N'MS_Description', @value=N'Completion lab: tenant-scoped customers', @level0type=N'SCHEMA', @level0name=N'lab', @level1type=N'TABLE', @level1name=N'customers';
EXEC sys.sp_addextendedproperty @name=N'MS_Description', @value=N'Display name; includes Unicode', @level0type=N'SCHEMA', @level0name=N'lab', @level1type=N'TABLE', @level1name=N'customers', @level2type=N'COLUMN', @level2name=N'customer_name';
CREATE USER completion_tester FOR LOGIN completion_tester WITH DEFAULT_SCHEMA=lab;
ALTER ROLE db_datareader ADD MEMBER completion_tester;
GRANT EXECUTE ON SCHEMA::lab TO completion_tester;
GRANT VIEW DEFINITION ON SCHEMA::lab TO completion_tester;
COMMIT;
GO

-- 最后一行必须是 READY，并且四个数量依次为 5、7、7、2。
SELECT 'READY' AS preparation,
    (SELECT COUNT(*) FROM lab.customers) AS customers,
    (SELECT COUNT(*) FROM lab.orders) AS orders,
    (SELECT COUNT(*) FROM lab.order_items) AS order_items,
    (SELECT COUNT(*) FROM archive.customers) AS archive_customers;
