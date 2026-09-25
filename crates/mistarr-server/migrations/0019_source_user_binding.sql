-- The user's choice of a source's binding, a platform or none, which automatic binding never changes.
ALTER TABLE sources RENAME COLUMN user_unbound TO user_binding;   -- 1 when the user chose the binding; 0 while it is automatic
ALTER TABLE sources ADD COLUMN bind_pending TEXT;   -- the binding asked for and not applied yet: 'automatic' | 'none' | 'platform:<id>'
UPDATE sources SET reason = 'Marked as not a game set. It is not bound automatically.'
  WHERE user_binding = 1 AND platform_id IS NULL AND state = 'unbound';
