# Shared Start-menu names for installation and catalog verification.
function Get-TiloWindowsAppName {
    param([Parameter(Mandatory)][string]$Application)
    switch ($Application) {
        'file-explorer' { 'Files' }
        'word-processor' { 'Word Processor' }
        'hex-editor' { 'Hex Editor' }
        'code-editor' { 'Code Editor' }
        'api-tester' { 'API Tester' }
        'diffmerge' { 'Diff and Merge' }
        'db-client' { 'Database Client' }
        'git-client' { 'Git Client' }
        'pdf-viewer' { 'PDF Viewer' }
        'video-player' { 'Video Player' }
        'music' { 'Music' }
        'ebook' { 'Books' }
        'totp' { 'Authenticator' }
        'typst-editor' { 'Typst' }
        default { (Get-Culture).TextInfo.ToTitleCase($Application) }
    }
}
