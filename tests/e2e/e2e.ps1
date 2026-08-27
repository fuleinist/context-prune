Set-Location G:\dev\projects\context-prune
Remove-Item tests\e2e\e2e.db -ErrorAction SilentlyContinue
$mock = Start-Process -FilePath node -ArgumentList 'tests/e2e/mock-upstream.cjs' -PassThru -WindowStyle Hidden
$proxy = Start-Process -FilePath '.\target\debug\context-prune.exe' -ArgumentList 'serve','--upstream','http://127.0.0.1:9999','--port','8787','--db','tests/e2e/e2e.db' -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 3
node tests/e2e/run.cjs
$code = $LASTEXITCODE
Stop-Process -Id $mock.Id -ErrorAction SilentlyContinue
Stop-Process -Id $proxy.Id -ErrorAction SilentlyContinue
Remove-Item tests\e2e\e2e.db -ErrorAction SilentlyContinue
exit $code
