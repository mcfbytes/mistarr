-- sources.reason holds a JSON code with its parameters, which the API words; see docs/DATA-MODEL.md "sources".
-- Each stored sentence becomes its code; other text is dropped and stored codes are kept.
UPDATE sources SET reason = CASE
  WHEN reason = 'No download client found. The file list is read once one is detected.'
    THEN '{"code":"no_client"}'
  WHEN reason = 'Waiting for the download client to read the file list.'
    THEN '{"code":"waiting_metadata"}'
  WHEN reason = 'Marked as not a game set. It is not bound automatically.'
    THEN '{"code":"ignored"}'
  WHEN reason LIKE 'The download client did not accept the source: %.'
    THEN json_object('code', 'client_refused', 'error', substr(reason, 48, length(reason) - 48))
  WHEN reason LIKE 'No platform matched %'
    THEN json_object('code', 'no_match',
      'percent', CAST(substr(reason, 21, instr(reason, '%') - 21) AS INTEGER),
      'suggested', CASE WHEN instr(reason, ' Its names suggest ') > 0 THEN suggested_platform_id END)
  WHEN reason LIKE 'Looks like %' AND suggested_platform_id IS NOT NULL
    THEN json_object('code', 'awaiting_dat', 'platform', suggested_platform_id)
  END
WHERE reason IS NOT NULL AND NOT json_valid(reason);
