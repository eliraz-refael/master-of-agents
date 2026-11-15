CREATE TRIGGER IF NOT EXISTS a_proj_ins AFTER INSERT ON projects BEGIN
  INSERT INTO activity(who,text) VALUES('brain','new project ' || NEW.name || ' (' || NEW.status || ')'); END;
CREATE TRIGGER IF NOT EXISTS a_proj_status AFTER UPDATE OF status ON projects WHEN OLD.status IS NOT NEW.status BEGIN
  INSERT INTO activity(who,text) VALUES('brain','project ' || NEW.name || ': ' || OLD.status || ' → ' || NEW.status); END;
CREATE TRIGGER IF NOT EXISTS a_task_ins AFTER INSERT ON tasks BEGIN
  INSERT INTO activity(who,text) VALUES('brain','new task "' || NEW.title || '"' ||
    coalesce(' in ' || (SELECT name FROM projects WHERE id=NEW.project_id), '')); END;
CREATE TRIGGER IF NOT EXISTS a_task_status AFTER UPDATE OF status ON tasks WHEN OLD.status IS NOT NEW.status BEGIN
  INSERT INTO activity(who,text) VALUES('brain','task "' || NEW.title || '": ' || OLD.status || ' → ' || NEW.status); END;
CREATE TRIGGER IF NOT EXISTS a_task_today AFTER UPDATE OF today ON tasks
  WHEN NEW.today IS NOT NULL AND OLD.today IS NOT NEW.today BEGIN
  INSERT INTO activity(who,text) VALUES('brain','added to today: "' || NEW.title || '"'); END;
CREATE TRIGGER IF NOT EXISTS a_note_ins AFTER INSERT ON notes BEGIN
  INSERT INTO activity(who,text) VALUES('brain','noted (' || NEW.kind || '): ' || substr(NEW.text,1,100)); END;
CREATE TRIGGER IF NOT EXISTS a_sugg_ins AFTER INSERT ON suggestions BEGIN
  INSERT INTO activity(who,text) VALUES('brain','suggests #' || NEW.id || ': ' || NEW.text); END;
CREATE TRIGGER IF NOT EXISTS a_sess_link AFTER UPDATE OF project_id ON sessions
  WHEN NEW.project_id IS NOT NULL AND OLD.project_id IS NOT NEW.project_id BEGIN
  INSERT INTO activity(who,text) VALUES('brain','linked session ' || substr(NEW.session_id,1,8) || ' → ' ||
    (SELECT name FROM projects WHERE id=NEW.project_id)); END;
CREATE TRIGGER IF NOT EXISTS a_event AFTER INSERT ON events WHEN NEW.kind != 'start' BEGIN
  INSERT INTO activity(who,text) VALUES(CASE NEW.kind WHEN 'ui' THEN 'you' ELSE 'session' END,
    CASE NEW.kind WHEN 'ui' THEN NEW.text ELSE
      replace(NEW.cwd, rtrim(NEW.cwd, replace(NEW.cwd,'/','')), '') || ' ' ||
      CASE NEW.kind WHEN 'prompt' THEN '› ' || substr(replace(NEW.text, char(10), ' '),1,90)
                    WHEN 'stop' THEN '✓ turn done' WHEN 'waiting' THEN '⚠ needs you'
                    WHEN 'end' THEN '○ ended' ELSE NEW.kind END END); END;
