#!/bin/sh

echo "[labyrinth] linpeas fallback enumeration"
echo "timestamp: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo

echo "== basic system =="
cat /etc/issue 2>/dev/null
cat /etc/os-release 2>/dev/null
uname -a 2>/dev/null
uname -r 2>/dev/null
arch 2>/dev/null
hostname 2>/dev/null
id 2>/dev/null
cat /etc/passwd 2>/dev/null | grep sh$ 2>/dev/null
echo

echo "== network =="
ifconfig 2>/dev/null || ip a 2>/dev/null
ss -tunlp 2>/dev/null || netstat -anp 2>/dev/null
route -n 2>/dev/null || ip route 2>/dev/null
resolvectl status 2>/dev/null || cat /etc/resolv.conf 2>/dev/null
echo

echo "== quick priv-esc hints =="
echo "sudo permissions:" && sudo -n -l 2>/dev/null
echo "suid binaries:" && find / -perm -4000 -type f 2>/dev/null | head -n 200
echo "writable /etc files:" && find /etc -maxdepth 2 -writable -type f 2>/dev/null | head -n 200
