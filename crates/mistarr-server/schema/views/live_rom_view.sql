CREATE VIEW live_rom_view AS
SELECT * FROM rom_view WHERE live AND NOT bios;
