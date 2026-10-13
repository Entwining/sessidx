INSERT INTO files(id,path,agent,dev,inode,size,mtime,prefix_hash,prefix_len,state_json)
VALUES
 (1,'/synthetic/transcript-1.jsonl','codex',0,1,0,'0','',0,'{}'),
 (2,'/synthetic/transcript-2.jsonl','codex',0,2,0,'0','',0,'{}'),
 (3,'/synthetic/transcript-3.jsonl','codex',0,3,0,'0','',0,'{}');
INSERT INTO sessions(file_id,agent,session_id,session_kind,session_kind_source,path,cwd,started_at)
SELECT id,agent,CASE WHEN id<=2 THEN 'shared-thread' ELSE 'other-thread' END,
 'main','synthetic',path,'/synthetic','2026-10-01' FROM files;
INSERT INTO event_rows(file_id,line_no,byte_off,byte_len,ordinal,raw_hash,ts,role,role_source,kind,kind_source,model_source,ok_source,line_role_first,canonical,call_ok_source)
VALUES
 (1,1,0,0,0,zeroblob(32),'2026-10-01','assistant','synthetic','tool_call','synthetic','none','none',1,1,'none'),
 (1,2,0,0,0,zeroblob(32),'2026-10-01','assistant','synthetic','tool_call','synthetic','none','none',1,1,'none'),
 (2,1,0,0,0,zeroblob(32),'2026-10-01','assistant','synthetic','tool_call','synthetic','none','none',1,1,'none'),
 (2,2,0,0,0,zeroblob(32),'2026-10-01','assistant','synthetic','tool_call','synthetic','none','none',1,1,'none'),
 (2,3,0,0,0,zeroblob(32),'2026-10-01','assistant','synthetic','tool_call','synthetic','none','none',1,1,'none'),
 (3,1,0,0,0,zeroblob(32),'2026-10-01','assistant','synthetic','tool_call','synthetic','none','none',1,1,'none');
