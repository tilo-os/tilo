# Shared Start-menu names for installation and catalog verification.
function Get-TiloWindowsAppName {
    param([Parameter(Mandatory)][string]$Application)
    switch ($Application) {
        'file-explorer' { 'Files' }
        'word-processor' { 'Word Processor' }
        'image-editor' { 'Image Editor' }
        'hex-editor' { 'Hex Editor' }
        'code-editor' { 'Code Editor' }
        'api-tester' { 'API Tester' }
        'diffmerge' { 'Diff and Merge' }
        'db-client' { 'Database Client' }
        'git-client' { 'Git Client' }
        'pdf-viewer' { 'PDF Viewer' }
        'ebook' { 'Books' }
        'totp' { 'Authenticator' }
        default { (Get-Culture).TextInfo.ToTitleCase($Application) }
    }
}
