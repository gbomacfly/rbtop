import subprocess, sys
n = int(sys.argv[1])
for _ in range(n):
    subprocess.Popen(["sleep", "3600"], executable="sleep", stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL, start_new_session=True, env={"ZZLOAD": "1"})
