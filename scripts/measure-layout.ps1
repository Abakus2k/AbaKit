<#
.SYNOPSIS
  Measures the theme's layout in headless Chrome, with long words in every building block.

.DESCRIPTION
  Opens a page that uses aba-theme.css (for example the hello example), replaces the content of
  its .page element with a fixture and measures it at each width. A width passes when

    - the document is not wider than the viewport (document.documentElement.scrollWidth),
    - at phone widths every tab of the bottom bar is inside the viewport, and
    - no word of 16 characters or less is broken across two lines.

  The fixture puts a 60-character web address and a 60-character unbroken word into an h1, a
  paragraph, a list item, the .kv pairs (next to a label of 16 characters), the .eyebrow,
  .badge, .error-box, .note and .btn-row blocks, and a .kv with real multi-word labels (the
  kind a cost list has).

  It also puts five flex rows of an app's own into the fixture, each a short label or button next
  to an ordinary multi-word sentence. `.main { overflow-wrap: anywhere }` lets a row that does not
  wrap squeeze the short side apart, so three rows carry the remedies the README names
  (flex-wrap: wrap on the row, flex-shrink: 0 on the label, flex-shrink: 0 on the button) and
  must pass. The other two are the hazard (no remedy; min-width: 0 on the text side, which does
  not help) and must squeeze their label: if they do not, the check is blind or the README's
  advice is stale, and the width fails.

  Chrome runs headless (no window) and is stopped at the end. It is driven over the DevTools
  protocol with Emulation.setDeviceMetricsOverride: a plain --window-size would only crop a
  layout that is too wide. Exit code 0 when every width passes, 1 otherwise.

  With -NoFixture the page is measured as it is (an app's own page, for example). With
  -ScreenshotDir one PNG per width is written: look at them, they show what a number cannot.

.EXAMPLE
  Start the example (cargo run --example hello), then, in the repository's folder:
  pwsh -File scripts/measure-layout.ps1 -Url http://127.0.0.1:8080/
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Url,
    # Viewport widths in CSS pixels, separated by commas.
    [string]$Widths = '320,390,900,1280',
    [string]$Chrome = 'C:\Program Files\Google\Chrome\Application\chrome.exe',
    [int]$DebugPort = 9333,
    # Measure the page as it is, without replacing its .page content by the fixture.
    [switch]$NoFixture,
    # When set, one PNG per width is written there (look at them, they are the proof).
    [string]$ScreenshotDir = ''
)

$ErrorActionPreference = 'Stop'

# Runs in the page. `expected` is the emulated viewport width.
$probe = @'
(function (expected, useFixture) {
  var LONG = 'abcdefghij'.repeat(6);
  var URL60 = 'https://example.invalid/projects/alpha/beta/gamma/delta/12345678';
  // Ordinary words, none longer than 16 characters, and wider than any row at any width.
  var SENTENCE = ('Ein ganz normaler Satz mit mehreren kurzen Woertern, der nicht neben das Label ' +
    'passt und deshalb auf mehrere Zeilen umbrechen muss. ').repeat(4);
  var fixture =
    '<div class="page-head"><h1>Hallo ' + LONG + '</h1>' +
    '<p class="sub">Adresse ' + URL60 + ' ist erreichbar.</p></div>' +
    '<div class="card"><h2>Karte ' + LONG + '</h2>' +
    '<p>Ein Pfad D:\\Archiv\\Dokumente\\' + LONG + ' und ein paar kurze Woerter.</p>' +
    '<ul><li>Listenpunkt ' + URL60 + '</li></ul>' +
    '<dl class="kv">' +
    '<div><dt>Gelesen</dt><dd>' + URL60 + '</dd></div>' +
    '<div><dt>Donaudampfschiff</dt><dd>' + LONG + '</dd></div>' +
    '<div><dt>Benutzer</dt><dd>' + LONG + '</dd></div>' +
    '<div><dt>Name</dt><dd>Anna</dd></div></dl></div>' +
    '<div class="card"><dl class="kv">' +
    '<div><dt>Einkauf gesamt</dt><dd>48,20 \u20ac</dd></div>' +
    '<div><dt>davon f\u00fcr Reste (Packungsgr\u00f6\u00dfen)</dt><dd>12,34 \u20ac</dd></div>' +
    '<div><dt>Nicht zugeordnete Zutaten (gesch\u00e4tzt)</dt><dd>3,10 \u20ac</dd></div>' +
    '<div><dt>pro Portion</dt><dd>2,41 \u20ac</dd></div></dl></div>' +
    '<div class="card">' +
    '<p class="eyebrow"><span>Woche 47</span><span>' + LONG + '</span></p>' +
    '<div class="badges"><span class="badge">' + LONG + '</span><span class="badge">kurz</span></div>' +
    '<div class="error-box"><div class="error-title"><svg class="icon" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"></circle></svg>' +
    '<span>Fehler bei ' + URL60 + '</span></div><ul class="error-details"><li>' + LONG + '</li></ul></div>' +
    '<p class="note">Hinweis zu ' + URL60 + '</p>' +
    '<div class="btn-row"><a class="btn" href="#">Speichern</a><a class="btn" href="#">' + LONG + '</a></div>' +
    '</div>' +
    // Rows of an app's own: a short label or button next to an ordinary sentence. The theme's
    // `.main { overflow-wrap: anywhere }` lowers every flex item's minimum width to one letter,
    // so a row that does not wrap squeezes the short side apart unless it has a remedy. The
    // first three rows carry the remedies the README names and must pass; the last two are the
    // hazard (no remedy, and min-width: 0 on the text side, which does NOT help) and must break,
    // or this check is blind and the README's advice is stale.
    '<div class="card"><p class="eyebrow"><span>Zeilen der App</span></p>' +
    '<div class="t-row t-wrap"><span>Gelesen</span><span>' + SENTENCE + '</span></div>' +
    '<div class="t-row t-keep"><span>Gelesen</span><span>' + SENTENCE + '</span></div>' +
    '<div class="t-row t-keep"><a class="btn" href="#">Bearbeiten</a><span>' + SENTENCE + '</span></div>' +
    '<div class="t-row" data-hazard="none"><span>Gelesen</span><span>' + SENTENCE + '</span></div>' +
    '<div class="t-row t-minw" data-hazard="minwidth"><span>Gelesen</span><span>' + SENTENCE + '</span></div>' +
    '</div>';
  if (useFixture) {
    document.querySelector('.page').innerHTML = fixture;
    // Through the CSSOM: the page's CSP (default-src 'self') refuses a <style> element and a
    // style attribute, but not a constructed stylesheet.
    var rows = new CSSStyleSheet();
    rows.replaceSync(
      '.t-row { display: flex; gap: 8px; align-items: flex-start; margin: 8px 0; }' +
      '.t-wrap { flex-wrap: wrap; }' +
      '.t-keep > :first-child { flex-shrink: 0; }' +
      '.t-minw > :last-child { min-width: 0; }');
    document.adoptedStyleSheets = document.adoptedStyleSheets.concat([rows]);
  }
  var main = document.querySelector('main');
  var root = document.documentElement;

  var wide = [];
  main.querySelectorAll('*').forEach(function (el) {
    var r = el.getBoundingClientRect();
    if (r.right > expected + 0.5) {
      wide.push(el.tagName.toLowerCase() + (el.className && el.className.baseVal === undefined ? '.' + el.className : ''));
    }
  });

  var tabsOutside = 0;
  var tabs = document.querySelectorAll('.nav-link');
  if (expected < 900) {
    tabs.forEach(function (tab) {
      var r = tab.getBoundingClientRect();
      if (r.left < -0.5 || r.right > expected + 0.5) tabsOutside += 1;
    });
  }

  // The words of 16 characters or less under `scope` that are broken across two lines. The
  // hazard rows (data-hazard) are skipped unless the scope is one of them.
  function brokenWords(scope) {
    var found = [];
    var walker = document.createTreeWalker(scope, NodeFilter.SHOW_TEXT, {
      acceptNode: function (n) {
        return scope === main && n.parentElement.closest('[data-hazard]')
          ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT;
      }
    });
    for (var node; (node = walker.nextNode());) {
      var re = /\S+/g, m;
      while ((m = re.exec(node.nodeValue))) {
        if (m[0].length > 16) continue;
        var range = document.createRange();
        range.setStart(node, m.index);
        range.setEnd(node, m.index + m[0].length);
        var tops = {};
        Array.prototype.forEach.call(range.getClientRects(), function (q) { tops[Math.round(q.top)] = 1; });
        if (Object.keys(tops).length > 1) found.push(m[0]);
      }
    }
    return found;
  }
  var broken = brokenWords(main);

  // The hazard rows must squeeze their label: that is what the remedy rows are measured against.
  var hazard = {};
  document.querySelectorAll('[data-hazard]').forEach(function (row) {
    hazard[row.getAttribute('data-hazard')] = brokenWords(row).length;
  });

  return {
    scrollWidth: root.scrollWidth,
    clientWidth: root.clientWidth,
    tabs: tabs.length,
    tabsOutside: tabsOutside,
    wide: wide.slice(0, 6),
    broken: broken,
    hazard: hazard
  };
})
'@

$script:socket = $null
$script:nextId = 0

function Receive-Message {
    $buffer = New-Object byte[] 65536
    $stream = [System.IO.MemoryStream]::new()
    do {
        $part = $script:socket.ReceiveAsync([ArraySegment[byte]]::new($buffer), [Threading.CancellationToken]::None).GetAwaiter().GetResult()
        if ($part.MessageType -eq 'Close') { throw 'Chrome closed the connection' }
        $stream.Write($buffer, 0, $part.Count)
    } until ($part.EndOfMessage)
    [Text.Encoding]::UTF8.GetString($stream.ToArray())
}

function Send-Cdp([string]$Method, [hashtable]$Params = @{}) {
    $script:nextId++
    $message = @{ id = $script:nextId; method = $Method; params = $Params } | ConvertTo-Json -Depth 20 -Compress
    $bytes = [Text.Encoding]::UTF8.GetBytes($message)
    $null = $script:socket.SendAsync([ArraySegment[byte]]::new($bytes), 'Text', $true, [Threading.CancellationToken]::None).GetAwaiter().GetResult()
    while ($true) {
        $reply = Receive-Message | ConvertFrom-Json -Depth 50
        if ($reply.id -eq $script:nextId) {
            if ($reply.error) { throw "$Method failed: $($reply.error.message)" }
            return $reply.result
        }
    }
}

function Get-PageValue([string]$Expression) {
    $result = Send-Cdp 'Runtime.evaluate' @{ expression = $Expression; returnByValue = $true }
    if ($result.exceptionDetails) { throw "script error: $($result.exceptionDetails.text) $($result.exceptionDetails.exception.description)" }
    $result.result.value
}

$profileDir = Join-Path ([IO.Path]::GetTempPath()) ("measure-layout-" + [guid]::NewGuid().ToString('N'))
$chromeArgs = @(
    '--headless=new', "--remote-debugging-port=$DebugPort", "--user-data-dir=$profileDir",
    '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--hide-scrollbars', 'about:blank'
)
$process = Start-Process -FilePath $Chrome -ArgumentList $chromeArgs -WindowStyle Hidden -PassThru
$failed = $false
try {
    $target = $null
    $lastError = ''
    foreach ($attempt in 1..150) {
        try {
            # Invoke-RestMethod hands the whole JSON array over as one object: loop over it.
            $targets = Invoke-RestMethod "http://127.0.0.1:$DebugPort/json/list"
            foreach ($candidate in $targets) {
                if ($candidate.type -eq 'page') { $target = $candidate; break }
            }
            if ($target) { break }
            $lastError = 'the browser listed no page'
        } catch { $lastError = $_.Exception.Message }
        Start-Sleep -Milliseconds 200
    }
    if (-not $target) { throw "Chrome did not offer a page on port ${DebugPort}: $lastError" }
    $script:socket = [Net.WebSockets.ClientWebSocket]::new()
    $null = $script:socket.ConnectAsync([Uri]$target.webSocketDebuggerUrl, [Threading.CancellationToken]::None).GetAwaiter().GetResult()
    $null = Send-Cdp 'Page.enable'

    if ($ScreenshotDir) { New-Item -ItemType Directory -Force $ScreenshotDir | Out-Null }
    $rows = foreach ($width in ($Widths -split '[,\s]+' | Where-Object { $_ } | ForEach-Object { [int]$_ })) {
        $null = Send-Cdp 'Emulation.setDeviceMetricsOverride' @{
            width = $width; height = 800; deviceScaleFactor = 1; mobile = ($width -lt 600)
        }
        $null = Send-Cdp 'Page.navigate' @{ url = $Url }
        $ready = $false
        foreach ($attempt in 1..100) {
            if ((Get-PageValue "document.readyState + '|' + (location.href !== 'about:blank')") -eq 'complete|True') { $ready = $true; break }
            Start-Sleep -Milliseconds 100
        }
        if (-not $ready) { throw "the page $Url did not finish loading" }
        $m = Get-PageValue "($probe)($width, $(if ($NoFixture) { 'false' } else { 'true' }))"
        if ($ScreenshotDir) {
            $shot = Send-Cdp 'Page.captureScreenshot' @{ format = 'png'; captureBeyondViewport = $true }
            [IO.File]::WriteAllBytes((Join-Path $ScreenshotDir "layout-$width.png"), [Convert]::FromBase64String($shot.data))
        }
        # The fixture's two hazard rows (no remedy, min-width: 0 only) must squeeze their label;
        # when they do not, the remedy rows prove nothing and the README's advice is stale.
        $blind = (-not $NoFixture) -and (($m.hazard.none -lt 1) -or ($m.hazard.minwidth -lt 1))
        $ok = ($m.scrollWidth -le $width) -and ($m.tabsOutside -eq 0) -and (@($m.broken).Count -eq 0) -and (-not $blind)
        if (-not $ok) { $failed = $true }
        [pscustomobject]@{
            Width       = $width
            ScrollWidth = $m.scrollWidth
            TabsOutside = "$($m.tabsOutside) of $($m.tabs)"
            Broken      = (@($m.broken) -join ' ')
            WiderThanViewport = (@($m.wide) -join ' ')
            HazardRows  = if ($NoFixture) { '-' } elseif ($blind) { 'NOT squeezed (blind)' } else { 'squeezed, as expected' }
            Result      = if ($ok) { 'ok' } else { 'FAIL' }
        }
    }
    $rows | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
}
finally {
    if ($script:socket) { $script:socket.Dispose() }
    if ($process -and -not $process.HasExited) { & taskkill.exe /PID $process.Id /T /F 2>&1 | Out-Null }
    # Chrome's helper processes let go of the profile folder a moment after they are stopped.
    foreach ($attempt in 1..20) {
        Remove-Item -Recurse -Force $profileDir -ErrorAction SilentlyContinue
        if (-not (Test-Path $profileDir)) { break }
        Start-Sleep -Milliseconds 250
    }
    if (Test-Path $profileDir) { Write-Warning "could not remove $profileDir" }
}
if ($failed) { exit 1 }
