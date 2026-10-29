CREATE ROLE completion_tester LOGIN PASSWORD 'o9i8u7y6';
CREATE DATABASE completion_lab OWNER completion_tester;
\connect completion_lab
SET ROLE completion_tester;

-- 仅首次准备时整份执行；后续测试不要重跑。
-- 遇到同名库/用户/表即停止，不删除或覆盖现有对象。
-- 所有测试数据一次准备完成，执行方式见同目录《测试教程.md》。

BEGIN;
CREATE SCHEMA lab;
CREATE SCHEMA archive;
SET search_path TO lab, public;

CREATE TABLE tenants (
    tenant_id INT PRIMARY KEY,
    tenant_name VARCHAR(80) NOT NULL
);

CREATE TABLE customers (
    tenant_id INT NOT NULL,
    customer_id INT NOT NULL,
    customer_name VARCHAR(100) NOT NULL,
    email VARCHAR(160),
    region VARCHAR(30),
    credit_limit DECIMAL(12,2),
    created_at TIMESTAMP NOT NULL,
    CONSTRAINT pk_customers PRIMARY KEY (tenant_id, customer_id),
    CONSTRAINT fk_customer_tenant FOREIGN KEY (tenant_id) REFERENCES tenants(tenant_id)
);

CREATE TABLE departments (
    department_id INT PRIMARY KEY,
    department_name VARCHAR(80) NOT NULL
);

CREATE TABLE employees (
    employee_id INT PRIMARY KEY,
    employee_name VARCHAR(100) NOT NULL,
    department_id INT,
    manager_id INT,
    salary DECIMAL(12,2),
    CONSTRAINT fk_employee_department FOREIGN KEY (department_id) REFERENCES departments(department_id),
    CONSTRAINT fk_employee_manager FOREIGN KEY (manager_id) REFERENCES employees(employee_id)
);

CREATE TABLE categories (
    category_id INT PRIMARY KEY,
    parent_id INT,
    category_name VARCHAR(80) NOT NULL,
    CONSTRAINT fk_category_parent FOREIGN KEY (parent_id) REFERENCES categories(category_id)
);

CREATE TABLE products (
    product_id INT PRIMARY KEY,
    category_id INT,
    product_name VARCHAR(100) NOT NULL,
    unit_price DECIMAL(12,2) NOT NULL,
    sku VARCHAR(50) NOT NULL UNIQUE,
    CONSTRAINT fk_product_category FOREIGN KEY (category_id) REFERENCES categories(category_id)
);

CREATE TABLE orders (
    tenant_id INT NOT NULL,
    order_id INT NOT NULL,
    customer_id INT,
    sales_rep_id INT,
    status VARCHAR(20) NOT NULL,
    total_amount DECIMAL(12,2) NOT NULL,
    ordered_at TIMESTAMP NOT NULL,
    note VARCHAR(200),
    CONSTRAINT pk_orders PRIMARY KEY (tenant_id, order_id),
    CONSTRAINT fk_order_customer FOREIGN KEY (tenant_id, customer_id) REFERENCES customers(tenant_id, customer_id),
    CONSTRAINT fk_order_tenant FOREIGN KEY (tenant_id) REFERENCES tenants(tenant_id),
    CONSTRAINT fk_order_employee FOREIGN KEY (sales_rep_id) REFERENCES employees(employee_id),
    CONSTRAINT ck_order_status CHECK (status IN ('new','paid','cancelled'))
);

CREATE TABLE order_items (
    tenant_id INT NOT NULL,
    order_id INT NOT NULL,
    line_no INT NOT NULL,
    product_id INT NOT NULL,
    quantity INT NOT NULL,
    unit_price DECIMAL(12,2) NOT NULL,
    discount DECIMAL(12,2) NOT NULL,
    CONSTRAINT pk_order_items PRIMARY KEY (tenant_id, order_id, line_no),
    CONSTRAINT fk_item_order FOREIGN KEY (tenant_id, order_id) REFERENCES orders(tenant_id, order_id),
    CONSTRAINT fk_item_product FOREIGN KEY (product_id) REFERENCES products(product_id),
    CONSTRAINT ck_item_quantity CHECK (quantity > 0)
);

CREATE TABLE payments (
    payment_id INT PRIMARY KEY,
    tenant_id INT NOT NULL,
    order_id INT NOT NULL,
    amount DECIMAL(12,2) NOT NULL,
    paid_at TIMESTAMP,
    method VARCHAR(20),
    CONSTRAINT fk_payment_order FOREIGN KEY (tenant_id, order_id) REFERENCES orders(tenant_id, order_id)
);

CREATE TABLE audit_events (
    event_id INT PRIMARY KEY,
    entity_type VARCHAR(30) NOT NULL,
    entity_id INT,
    payload JSONB,
    created_at TIMESTAMP NOT NULL
);

CREATE TABLE "Order Details" (
    "Row ID" INT PRIMARY KEY,
    "select" VARCHAR(80),
    "MixedCase" VARCHAR(80),
    "客户名称" VARCHAR(80),
    "unit-price" DECIMAL(12,2)
);

CREATE TABLE "quote`bracket]double""" (
    "odd`col]name""" VARCHAR(80)
);

CREATE TABLE wide_metrics (
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

CREATE TABLE archive.customers (tenant_id INT NOT NULL, customer_id INT NOT NULL, archived_name VARCHAR(100), archive_reason VARCHAR(100), PRIMARY KEY (tenant_id, customer_id));

CREATE INDEX ix_orders_customer ON orders(tenant_id, customer_id);

INSERT INTO tenants (tenant_id, tenant_name) VALUES (1, 'North Lab');

INSERT INTO tenants (tenant_id, tenant_name) VALUES (2, 'South Lab');

INSERT INTO customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (1, 101, 'Alice Chen', 'alice@example.invalid', 'east', 10000, '2026-10-01 10:00:00');

INSERT INTO customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (1, 102, 'Bob Li', NULL, 'west', 0, '2026-10-02 10:00:00');

INSERT INTO customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (1, 103, '张三', 'zhang@example.invalid', 'east', NULL, '2026-10-03 10:00:00');

INSERT INTO customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (2, 101, 'Alice South', 'south@example.invalid', 'south', 5000, '2026-10-04 10:00:00');

INSERT INTO customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (2, 104, 'O''Neil', NULL, NULL, 500, '2026-10-05 10:00:00');

INSERT INTO departments (department_id, department_name) VALUES (10, 'Sales');

INSERT INTO departments (department_id, department_name) VALUES (20, 'Engineering');

INSERT INTO departments (department_id, department_name) VALUES (30, 'Empty Department');

INSERT INTO employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (1, 'Director', 10, NULL, 30000);

INSERT INTO employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (2, 'Sales A', 10, 1, 10000);

INSERT INTO employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (3, 'Sales B', 10, 1, 10000);

INSERT INTO employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (4, 'Lead', 20, 1, 20000);

INSERT INTO employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (5, 'Engineer', 20, 4, 15000);

INSERT INTO employees (employee_id, employee_name, department_id, manager_id, salary) VALUES (6, 'Unassigned', NULL, NULL, NULL);

INSERT INTO categories (category_id, parent_id, category_name) VALUES (1, NULL, 'All');

INSERT INTO categories (category_id, parent_id, category_name) VALUES (2, 1, 'Hardware');

INSERT INTO categories (category_id, parent_id, category_name) VALUES (3, 1, 'Software');

INSERT INTO categories (category_id, parent_id, category_name) VALUES (4, 2, 'Accessories');

INSERT INTO products (product_id, category_id, product_name, unit_price, sku) VALUES (11, 2, 'Keyboard', 100, 'KB-001');

INSERT INTO products (product_id, category_id, product_name, unit_price, sku) VALUES (12, 4, 'Cable', 20, 'CB-001');

INSERT INTO products (product_id, category_id, product_name, unit_price, sku) VALUES (13, 3, 'License', 300, 'SW-001');

INSERT INTO products (product_id, category_id, product_name, unit_price, sku) VALUES (14, 2, 'Mouse', 50, 'MS-001');

INSERT INTO products (product_id, category_id, product_name, unit_price, sku) VALUES (15, NULL, 'Unused Product', 0, 'UNUSED');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1001, 101, 2, 'paid', 140, '2026-10-10 10:00:00', 'two lines');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1002, 101, 2, 'new', 300, '2026-10-11 10:00:00', NULL);

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1003, 102, 3, 'cancelled', 50, '2026-10-12 10:00:00', 'cancelled');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1004, NULL, NULL, 'new', 0, '2026-10-13 10:00:00', 'guest, no lines');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (2, 1001, 101, 3, 'paid', 120, '2026-10-14 10:00:00', 'same order_id, different tenant');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (2, 1005, 104, NULL, 'paid', 300, '2026-10-15 10:00:00', 'split payment');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (2, 1006, 104, 2, 'new', 0, '2026-10-16 10:00:00', 'zero quantity not allowed; no items');

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1001, 1, 11, 1, 100, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1001, 2, 12, 2, 20, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1002, 1, 13, 1, 300, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1003, 1, 14, 1, 50, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (2, 1001, 1, 11, 1, 100, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (2, 1001, 2, 12, 1, 20, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (2, 1005, 1, 13, 1, 300, 0);

INSERT INTO payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (1, 1, 1001, 140, '2026-10-10 10:00:00', 'card');

INSERT INTO payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (2, 2, 1001, 120, '2026-10-14 10:00:00', 'cash');

INSERT INTO payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (3, 2, 1005, 100, '2026-10-15 10:00:00', 'card');

INSERT INTO payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (4, 2, 1005, 200, '2026-10-16 10:00:00', 'bank');

INSERT INTO audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (1, 'order', 1001, '{"source":"web","tags":["vip","new"]}', '2026-10-10 10:00:00');

INSERT INTO audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (2, 'order', 1002, '{"source":"mobile","score":0}', '2026-10-11 10:00:00');

INSERT INTO audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (3, 'customer', 103, NULL, '2026-10-12 10:00:00');

INSERT INTO audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (4, 'order', 1005, '{"source":null,"nested":{"ok":true}}', '2026-10-15 10:00:00');

INSERT INTO "Order Details" ("Row ID", "select", "MixedCase", "客户名称", "unit-price") VALUES (1, 'keyword column', 'Upper', '张三', 12.5);

INSERT INTO "Order Details" ("Row ID", "select", "MixedCase", "客户名称", "unit-price") VALUES (2, 'second', NULL, '李四', 0);

INSERT INTO "quote`bracket]double""" ("odd`col]name""") VALUES ('escaped identifier');

INSERT INTO wide_metrics (metric_id, metric_01, metric_02, metric_03, metric_04, metric_05, metric_06, metric_07, metric_08, metric_09, metric_10, metric_11, metric_12, metric_13, metric_14, metric_15, metric_16, metric_17, metric_18, metric_19, metric_20, metric_21, metric_22, metric_23, metric_24, metric_25, metric_26, metric_27, metric_28, metric_29, metric_30, metric_31, metric_32, metric_33, metric_34, metric_35, metric_36, metric_37, metric_38, metric_39, metric_40, metric_41, metric_42, metric_43, metric_44, metric_45, metric_46, metric_47, metric_48) VALUES (1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48);

INSERT INTO archive.customers (tenant_id, customer_id, archived_name, archive_reason) VALUES (1, 101, 'Old Alice', 'merged');

INSERT INTO archive.customers (tenant_id, customer_id, archived_name, archive_reason) VALUES (9, 999, 'Legacy', 'retired');

CREATE VIEW v_order_summary AS
SELECT o.tenant_id, o.order_id, c.customer_name, o.status, o.total_amount,
       COUNT(i.line_no) AS line_count
FROM orders o
LEFT JOIN customers c ON c.tenant_id=o.tenant_id AND c.customer_id=o.customer_id
LEFT JOIN order_items i ON i.tenant_id=o.tenant_id AND i.order_id=o.order_id
GROUP BY o.tenant_id, o.order_id, c.customer_name, o.status, o.total_amount;

CREATE VIEW v_customer_totals AS
SELECT c.tenant_id, c.customer_id, c.customer_name, COUNT(o.order_id) AS order_count,
       COALESCE(SUM(o.total_amount),0) AS spent
FROM customers c
LEFT JOIN orders o ON o.tenant_id=c.tenant_id AND o.customer_id=c.customer_id
GROUP BY c.tenant_id,c.customer_id,c.customer_name;

COMMENT ON TABLE customers IS 'Completion lab: tenant-scoped customers';
COMMENT ON COLUMN customers.customer_name IS 'Display name; includes Unicode';
CREATE FUNCTION lab_discount(p_amount numeric) RETURNS numeric LANGUAGE SQL IMMUTABLE AS $$ SELECT p_amount * 0.9 $$;
CREATE FUNCTION lab_discount(p_amount numeric, p_rate numeric) RETURNS numeric LANGUAGE SQL IMMUTABLE AS $$ SELECT p_amount * (1-p_rate) $$;
CREATE PROCEDURE lab_touch(IN p_id integer) LANGUAGE plpgsql AS $$ BEGIN PERFORM p_id; END $$;
COMMIT;

ALTER ROLE completion_tester IN DATABASE completion_lab SET search_path=lab,public;

-- 最后一行必须是 READY，并且四个数量依次为 5、7、7、2。
SELECT 'READY' AS preparation,
    (SELECT COUNT(*) FROM lab.customers) AS customers,
    (SELECT COUNT(*) FROM lab.orders) AS orders,
    (SELECT COUNT(*) FROM lab.order_items) AS order_items,
    (SELECT COUNT(*) FROM archive.customers) AS archive_customers;
