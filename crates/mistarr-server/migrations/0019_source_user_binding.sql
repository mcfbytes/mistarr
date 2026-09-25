-- The user's choice of a source's binding, a platform or none, which automatic binding never changes.
ALTER TABLE sources RENAME COLUMN user_unbound TO user_binding;   -- 1 when the user chose the binding; 0 while it is automatic
