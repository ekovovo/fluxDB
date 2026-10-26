-- 仅首次准备时整份执行；后续测试不要重跑。
-- 遇到同名库/用户/表即停止，不删除或覆盖现有对象。
-- 所有测试数据一次准备完成，执行方式见同目录《测试教程.md》。

WHENEVER OSERROR EXIT FAILURE ROLLBACK
WHENEVER SQLERROR EXIT SQL.SQLCODE ROLLBACK
SET DEFINE OFF
-- 由 SQL*Plus SYSDBA 在 FREEPDB1 中一次准备两个专用 Schema。
ALTER SESSION SET CONTAINER=FREEPDB1;
CREATE USER COMPLETION_LAB IDENTIFIED BY "o9i8u7y6" DEFAULT TABLESPACE USERS QUOTA 100M ON USERS;
CREATE USER COMPLETION_ARCHIVE IDENTIFIED BY "o9i8u7y6" DEFAULT TABLESPACE USERS QUOTA 10M ON USERS;
GRANT CREATE SESSION, CREATE TABLE, CREATE VIEW, CREATE PROCEDURE, CREATE SYNONYM TO COMPLETION_LAB;
CREATE TABLE COMPLETION_ARCHIVE.customers (
    tenant_id NUMBER(10) NOT NULL,
    customer_id NUMBER(10) NOT NULL,
    archived_name VARCHAR2(100 CHAR),
    archive_reason VARCHAR2(100 CHAR),
    PRIMARY KEY (tenant_id, customer_id)
);
INSERT INTO COMPLETION_ARCHIVE.customers VALUES (1,101,'Old Alice','merged');
INSERT INTO COMPLETION_ARCHIVE.customers VALUES (9,999,'Legacy','retired');
COMMIT;
GRANT SELECT ON COMPLETION_ARCHIVE.customers TO COMPLETION_LAB;
ALTER SESSION SET CURRENT_SCHEMA=COMPLETION_LAB;


CREATE TABLE tenants (
    tenant_id NUMBER(10) PRIMARY KEY,
    tenant_name VARCHAR2(80 CHAR) NOT NULL
);

CREATE TABLE customers (
    tenant_id NUMBER(10) NOT NULL,
    customer_id NUMBER(10) NOT NULL,
    customer_name VARCHAR2(100 CHAR) NOT NULL,
    email VARCHAR2(160 CHAR),
    region VARCHAR2(30 CHAR),
    credit_limit NUMBER(12,2),
    created_at TIMESTAMP NOT NULL,
    CONSTRAINT pk_customers PRIMARY KEY (tenant_id, customer_id),
    CONSTRAINT fk_customer_tenant FOREIGN KEY (tenant_id) REFERENCES tenants(tenant_id)
);

CREATE TABLE departments (
    department_id NUMBER(10) PRIMARY KEY,
    department_name VARCHAR2(80 CHAR) NOT NULL
);

CREATE TABLE employees (
    employee_id NUMBER(10) PRIMARY KEY,
    employee_name VARCHAR2(100 CHAR) NOT NULL,
    department_id NUMBER(10),
    manager_id NUMBER(10),
    salary NUMBER(12,2),
    CONSTRAINT fk_employee_department FOREIGN KEY (department_id) REFERENCES departments(department_id),
    CONSTRAINT fk_employee_manager FOREIGN KEY (manager_id) REFERENCES employees(employee_id)
);

CREATE TABLE categories (
    category_id NUMBER(10) PRIMARY KEY,
    parent_id NUMBER(10),
    category_name VARCHAR2(80 CHAR) NOT NULL,
    CONSTRAINT fk_category_parent FOREIGN KEY (parent_id) REFERENCES categories(category_id)
);

CREATE TABLE products (
    product_id NUMBER(10) PRIMARY KEY,
    category_id NUMBER(10),
    product_name VARCHAR2(100 CHAR) NOT NULL,
    unit_price NUMBER(12,2) NOT NULL,
    sku VARCHAR2(50 CHAR) NOT NULL UNIQUE,
    CONSTRAINT fk_product_category FOREIGN KEY (category_id) REFERENCES categories(category_id)
);

CREATE TABLE orders (
    tenant_id NUMBER(10) NOT NULL,
    order_id NUMBER(10) NOT NULL,
    customer_id NUMBER(10),
    sales_rep_id NUMBER(10),
    status VARCHAR2(20 CHAR) NOT NULL,
    total_amount NUMBER(12,2) NOT NULL,
    ordered_at TIMESTAMP NOT NULL,
    note VARCHAR2(200 CHAR),
    CONSTRAINT pk_orders PRIMARY KEY (tenant_id, order_id),
    CONSTRAINT fk_order_customer FOREIGN KEY (tenant_id, customer_id) REFERENCES customers(tenant_id, customer_id),
    CONSTRAINT fk_order_tenant FOREIGN KEY (tenant_id) REFERENCES tenants(tenant_id),
    CONSTRAINT fk_order_employee FOREIGN KEY (sales_rep_id) REFERENCES employees(employee_id),
    CONSTRAINT ck_order_status CHECK (status IN ('new','paid','cancelled'))
);

CREATE TABLE order_items (
    tenant_id NUMBER(10) NOT NULL,
    order_id NUMBER(10) NOT NULL,
    line_no NUMBER(10) NOT NULL,
    product_id NUMBER(10) NOT NULL,
    quantity NUMBER(10) NOT NULL,
    unit_price NUMBER(12,2) NOT NULL,
    discount NUMBER(12,2) NOT NULL,
    CONSTRAINT pk_order_items PRIMARY KEY (tenant_id, order_id, line_no),
    CONSTRAINT fk_item_order FOREIGN KEY (tenant_id, order_id) REFERENCES orders(tenant_id, order_id),
    CONSTRAINT fk_item_product FOREIGN KEY (product_id) REFERENCES products(product_id),
    CONSTRAINT ck_item_quantity CHECK (quantity > 0)
);

CREATE TABLE payments (
    payment_id NUMBER(10) PRIMARY KEY,
    tenant_id NUMBER(10) NOT NULL,
    order_id NUMBER(10) NOT NULL,
    amount NUMBER(12,2) NOT NULL,
    paid_at TIMESTAMP,
    method VARCHAR2(20 CHAR),
    CONSTRAINT fk_payment_order FOREIGN KEY (tenant_id, order_id) REFERENCES orders(tenant_id, order_id)
);

CREATE TABLE audit_events (
    event_id NUMBER(10) PRIMARY KEY,
    entity_type VARCHAR2(30 CHAR) NOT NULL,
    entity_id NUMBER(10),
    payload CLOB,
    created_at TIMESTAMP NOT NULL
);

CREATE TABLE "Order Details" (
    "Row ID" NUMBER(10) PRIMARY KEY,
    "select" VARCHAR2(80 CHAR),
    "MixedCase" VARCHAR2(80 CHAR),
    "客户名称" VARCHAR2(80 CHAR),
    "unit-price" NUMBER(12,2)
);

CREATE TABLE "quote`bracket]double" (
    "odd`col]name" VARCHAR2(80 CHAR)
);

CREATE TABLE wide_metrics (
    metric_id NUMBER(10) PRIMARY KEY,
    metric_01 NUMBER(12,2),
    metric_02 NUMBER(12,2),
    metric_03 NUMBER(12,2),
    metric_04 NUMBER(12,2),
    metric_05 NUMBER(12,2),
    metric_06 NUMBER(12,2),
    metric_07 NUMBER(12,2),
    metric_08 NUMBER(12,2),
    metric_09 NUMBER(12,2),
    metric_10 NUMBER(12,2),
    metric_11 NUMBER(12,2),
    metric_12 NUMBER(12,2),
    metric_13 NUMBER(12,2),
    metric_14 NUMBER(12,2),
    metric_15 NUMBER(12,2),
    metric_16 NUMBER(12,2),
    metric_17 NUMBER(12,2),
    metric_18 NUMBER(12,2),
    metric_19 NUMBER(12,2),
    metric_20 NUMBER(12,2),
    metric_21 NUMBER(12,2),
    metric_22 NUMBER(12,2),
    metric_23 NUMBER(12,2),
    metric_24 NUMBER(12,2),
    metric_25 NUMBER(12,2),
    metric_26 NUMBER(12,2),
    metric_27 NUMBER(12,2),
    metric_28 NUMBER(12,2),
    metric_29 NUMBER(12,2),
    metric_30 NUMBER(12,2),
    metric_31 NUMBER(12,2),
    metric_32 NUMBER(12,2),
    metric_33 NUMBER(12,2),
    metric_34 NUMBER(12,2),
    metric_35 NUMBER(12,2),
    metric_36 NUMBER(12,2),
    metric_37 NUMBER(12,2),
    metric_38 NUMBER(12,2),
    metric_39 NUMBER(12,2),
    metric_40 NUMBER(12,2),
    metric_41 NUMBER(12,2),
    metric_42 NUMBER(12,2),
    metric_43 NUMBER(12,2),
    metric_44 NUMBER(12,2),
    metric_45 NUMBER(12,2),
    metric_46 NUMBER(12,2),
    metric_47 NUMBER(12,2),
    metric_48 NUMBER(12,2)
);

CREATE INDEX ix_orders_customer ON orders(tenant_id, customer_id);

INSERT INTO tenants (tenant_id, tenant_name) VALUES (1, 'North Lab');

INSERT INTO tenants (tenant_id, tenant_name) VALUES (2, 'South Lab');

INSERT INTO customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (1, 101, 'Alice Chen', 'alice@example.invalid', 'east', 10000, TO_TIMESTAMP('2026-10-01 10:00:00', 'YYYY-MM-DD HH24:MI:SS'));

INSERT INTO customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (1, 102, 'Bob Li', NULL, 'west', 0, TO_TIMESTAMP('2026-10-02 10:00:00', 'YYYY-MM-DD HH24:MI:SS'));

INSERT INTO customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (1, 103, '张三', 'zhang@example.invalid', 'east', NULL, TO_TIMESTAMP('2026-10-03 10:00:00', 'YYYY-MM-DD HH24:MI:SS'));

INSERT INTO customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (2, 101, 'Alice South', 'south@example.invalid', 'south', 5000, TO_TIMESTAMP('2026-10-04 10:00:00', 'YYYY-MM-DD HH24:MI:SS'));

INSERT INTO customers (tenant_id, customer_id, customer_name, email, region, credit_limit, created_at) VALUES (2, 104, 'O''Neil', NULL, NULL, 500, TO_TIMESTAMP('2026-10-05 10:00:00', 'YYYY-MM-DD HH24:MI:SS'));

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

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1001, 101, 2, 'paid', 140, TO_TIMESTAMP('2026-10-10 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), 'two lines');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1002, 101, 2, 'new', 300, TO_TIMESTAMP('2026-10-11 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), NULL);

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1003, 102, 3, 'cancelled', 50, TO_TIMESTAMP('2026-10-12 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), 'cancelled');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (1, 1004, NULL, NULL, 'new', 0, TO_TIMESTAMP('2026-10-13 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), 'guest, no lines');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (2, 1001, 101, 3, 'paid', 120, TO_TIMESTAMP('2026-10-14 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), 'same order_id, different tenant');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (2, 1005, 104, NULL, 'paid', 300, TO_TIMESTAMP('2026-10-15 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), 'split payment');

INSERT INTO orders (tenant_id, order_id, customer_id, sales_rep_id, status, total_amount, ordered_at, note) VALUES (2, 1006, 104, 2, 'new', 0, TO_TIMESTAMP('2026-10-16 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), 'zero quantity not allowed; no items');

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1001, 1, 11, 1, 100, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1001, 2, 12, 2, 20, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1002, 1, 13, 1, 300, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (1, 1003, 1, 14, 1, 50, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (2, 1001, 1, 11, 1, 100, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (2, 1001, 2, 12, 1, 20, 0);

INSERT INTO order_items (tenant_id, order_id, line_no, product_id, quantity, unit_price, discount) VALUES (2, 1005, 1, 13, 1, 300, 0);

INSERT INTO payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (1, 1, 1001, 140, TO_TIMESTAMP('2026-10-10 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), 'card');

INSERT INTO payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (2, 2, 1001, 120, TO_TIMESTAMP('2026-10-14 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), 'cash');

INSERT INTO payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (3, 2, 1005, 100, TO_TIMESTAMP('2026-10-15 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), 'card');

INSERT INTO payments (payment_id, tenant_id, order_id, amount, paid_at, method) VALUES (4, 2, 1005, 200, TO_TIMESTAMP('2026-10-16 10:00:00', 'YYYY-MM-DD HH24:MI:SS'), 'bank');

INSERT INTO audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (1, 'order', 1001, '{"source":"web","tags":["vip","new"]}', TO_TIMESTAMP('2026-10-10 10:00:00', 'YYYY-MM-DD HH24:MI:SS'));

INSERT INTO audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (2, 'order', 1002, '{"source":"mobile","score":0}', TO_TIMESTAMP('2026-10-11 10:00:00', 'YYYY-MM-DD HH24:MI:SS'));

INSERT INTO audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (3, 'customer', 103, NULL, TO_TIMESTAMP('2026-10-12 10:00:00', 'YYYY-MM-DD HH24:MI:SS'));

INSERT INTO audit_events (event_id, entity_type, entity_id, payload, created_at) VALUES (4, 'order', 1005, '{"source":null,"nested":{"ok":true}}', TO_TIMESTAMP('2026-10-15 10:00:00', 'YYYY-MM-DD HH24:MI:SS'));

INSERT INTO "Order Details" ("Row ID", "select", "MixedCase", "客户名称", "unit-price") VALUES (1, 'keyword column', 'Upper', '张三', 12.5);

INSERT INTO "Order Details" ("Row ID", "select", "MixedCase", "客户名称", "unit-price") VALUES (2, 'second', NULL, '李四', 0);

INSERT INTO "quote`bracket]double" ("odd`col]name") VALUES ('escaped identifier');

INSERT INTO wide_metrics (metric_id, metric_01, metric_02, metric_03, metric_04, metric_05, metric_06, metric_07, metric_08, metric_09, metric_10, metric_11, metric_12, metric_13, metric_14, metric_15, metric_16, metric_17, metric_18, metric_19, metric_20, metric_21, metric_22, metric_23, metric_24, metric_25, metric_26, metric_27, metric_28, metric_29, metric_30, metric_31, metric_32, metric_33, metric_34, metric_35, metric_36, metric_37, metric_38, metric_39, metric_40, metric_41, metric_42, metric_43, metric_44, metric_45, metric_46, metric_47, metric_48) VALUES (1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48);

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
CREATE FUNCTION lab_discount(p_amount NUMBER) RETURN NUMBER DETERMINISTIC IS
BEGIN RETURN p_amount * 0.9; END;
/
CREATE PROCEDURE lab_touch(p_id IN NUMBER) IS
BEGIN NULL; END;
/
CREATE PACKAGE lab_pkg AS
  FUNCTION discount(p_amount NUMBER) RETURN NUMBER;
  FUNCTION discount(p_amount NUMBER, p_rate NUMBER) RETURN NUMBER;
END lab_pkg;
/
CREATE PACKAGE BODY lab_pkg AS
  FUNCTION discount(p_amount NUMBER) RETURN NUMBER IS
  BEGIN RETURN p_amount * 0.9; END;
  FUNCTION discount(p_amount NUMBER, p_rate NUMBER) RETURN NUMBER IS
  BEGIN RETURN p_amount * (1-p_rate); END;
END lab_pkg;
/
CREATE SYNONYM customer_alias FOR customers;
CREATE SYNONYM archive_customer_alias FOR COMPLETION_ARCHIVE.customers;
COMMIT;


DECLARE
    invalid_count PLS_INTEGER;
BEGIN
    SELECT COUNT(*) INTO invalid_count FROM all_objects
    WHERE owner='COMPLETION_LAB' AND status<>'VALID';
    IF invalid_count<>0 THEN
        RAISE_APPLICATION_ERROR(-20001,'Completion test objects contain compilation errors');
    END IF;
END;
/

-- 最后一行必须是 READY，并且四个数量依次为 5、7、7、2。
SELECT 'READY' AS preparation,
    (SELECT COUNT(*) FROM COMPLETION_LAB.customers) AS customers,
    (SELECT COUNT(*) FROM COMPLETION_LAB.orders) AS orders,
    (SELECT COUNT(*) FROM COMPLETION_LAB.order_items) AS order_items,
    (SELECT COUNT(*) FROM COMPLETION_ARCHIVE.customers) AS archive_customers FROM dual;
EXIT SUCCESS;
