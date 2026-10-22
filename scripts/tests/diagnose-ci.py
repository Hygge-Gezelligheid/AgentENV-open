import json,os,pathlib,re,shutil,subprocess,time
P=pathlib.Path
root=P('/tmp/ci-diag');evidence=root/'evidence'
base=P('config/default.toml').read_text()
old=next(p for p in (root/'assets').rglob('firecracker*') if p.is_file() and p.name!='fc-old.tgz' and os.access(p,os.X_OK))
results=[]
for name,old_kernel,old_fc,apf in [('current',False,False,False),('no-apf',False,False,True),('old-kernel',True,False,False),('old-fc',False,True,False),('baseline',True,True,False)]:
 d=root/name;d.mkdir()
 text=base.replace('home_path = "/var/lib/aenv"',f'home_path = "{d}/home"\nruntime_path = "{d}/run"')
 text=text.replace('[firecracker]','[firecracker]\nlog_level = "Info"'+(f'\nbinary_path = "{old}"' if old_fc else ''))
 if old_kernel:text=text.replace('[kernel]','[kernel]\nimage_path = "/tmp/ci-diag/assets/vmlinux-old"')
 if apf:text=re.sub(r'^(boot_args = ".*)(")$',r'\1 no-kvmapf\2',text,flags=re.M)
 config=d/'node.toml';config.write_text(text)
 env=dict(os.environ,AENV_CONFIG_PATH=str(config),AENV_HOME_PATH=str(d/'home'),AENV_RUNTIME_PATH=str(d/'run'),AENV_UBLK_DAEMON_BINARY_PATH=str(P(os.environ.get('CARGO_TARGET_DIR','target')).resolve()/'debug/uvm-ublk-daemon'))
 env['PATH']=str(P(env['AENV_UBLK_DAEMON_BINARY_PATH']).parent)+':'+env['PATH']
 cmd=['scripts/run-with-capabilities.sh','cargo','test','-p','agentenv','--test','integration','fc::microvm_lifecycle_and_snapshot_preserve_disk_state','--','--exact','--nocapture','--test-threads=1']
 started=time.monotonic()
 with (evidence/(name+'.log')).open('w') as log:
  try:code=subprocess.run(cmd,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=220).returncode
  except subprocess.TimeoutExpired:code=124
 row={'case':name,'exit':code,'seconds':time.monotonic()-started};results.append(row);print(json.dumps(row),flush=True)
 serial=d/'home/logs/serial'
 if serial.exists():shutil.copytree(serial,evidence/(name+'-serial'))
 # Preserve only config and serial logs; state contains generated credentials.
 shutil.copy(config,evidence/(name+'.toml'))
 (evidence/'results.json').write_text(json.dumps(results,indent=2))
