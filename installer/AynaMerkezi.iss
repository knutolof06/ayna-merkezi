; Ayna Merkezi kurulum programı (Inno Setup 6). CI derler:
;   ISCC /DAppVer=<sürüm> installer\AynaMerkezi.iss   ->  AynaMerkezi-Setup.exe (depo kökünde)
; Kaynak klasör (SrcDir) iPhoneAyna.exe ile bin\uxplay\ içerir (iş akışının "Package" adımı hazırlar).
;
; Kullanıcı başına kurulur (yönetici izni gerekmez): uygulama kendini güncellerken exe'yi değiştirebilmeli,
; Program Files'ta bunu yapamazdı. Kaldırma kaydı HKCU'dadır; Denetim Masası > Programlar ve Özellikler
; ve Ayarlar > Uygulamalar ikisi de gösterir. Uygulama güncellendikçe DisplayVersion'ı kendisi yazar
; (src/install.rs; AppId orada da aynı).

#ifndef AppVer
  #define AppVer "0"
#endif
#ifndef SrcDir
  #define SrcDir "..\AynaMerkezi"
#endif

[Setup]
AppId={{15E0CF97-DA98-430C-BCFB-369CC239E8A0}
AppName=Ayna Merkezi
AppVersion={#AppVer}
AppVerName=Ayna Merkezi {#AppVer}
AppPublisher=Ayna Merkezi
AppPublisherURL=https://github.com/knutolof06/ayna-merkezi
AppSupportURL=https://github.com/knutolof06/ayna-merkezi/issues
AppUpdatesURL=https://github.com/knutolof06/ayna-merkezi/releases
VersionInfoVersion={#AppVer}.0.0.0
DefaultDirName={localappdata}\Programs\Ayna Merkezi
DefaultGroupName=Ayna Merkezi
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..
OutputBaseFilename=AynaMerkezi-Setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\iPhoneAyna.exe
UninstallDisplayName=Ayna Merkezi
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; Çalışan uygulamayı kurulum/kaldırma kendisi kapatır (aşağıda CloseApp).
CloseApplications=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "tr"; MessagesFile: "compiler:Languages\Turkish.isl"

[CustomMessages]
en.AutoStart=Start with Windows
tr.AutoStart=Windows ile başlat
en.RemoveSettings=Also remove the settings of Ayna Merkezi (Mac pairing, window sizes, language)?
tr.RemoveSettings=Ayna Merkezi ayarları da (Mac eşleşmesi, pencere boyutları, dil) silinsin mi?

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "autostart"; Description: "{cm:AutoStart}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SrcDir}\iPhoneAyna.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\bin\*"; DestDir: "{app}\bin"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Ayna Merkezi"; Filename: "{app}\iPhoneAyna.exe"
Name: "{autodesktop}\Ayna Merkezi"; Filename: "{app}\iPhoneAyna.exe"; Tasks: desktopicon

[Registry]
; Uygulamanın kendi "Windows ile başlat" ayarıyla aynı değer (src/util.rs set_autostart).
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "iPhoneAyna"; ValueData: """{app}\iPhoneAyna.exe"" --arka-plan"; Tasks: autostart

[Run]
Filename: "{app}\iPhoneAyna.exe"; Description: "{cm:LaunchProgram,Ayna Merkezi}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Yalnız bize ait olanlar silinir (kullanıcı içinde başka dosyalar olan bir klasör seçmiş olabilir):
; güncelleyicinin bıraktığı dosyalar ve AirPlay alıcısının klasörü.
Type: files; Name: "{app}\iPhoneAyna.guncelleme"
Type: files; Name: "{app}\iPhoneAyna.guncelleme.tmp"
Type: files; Name: "{app}\iPhoneAyna.eski.exe"
Type: files; Name: "{app}\iPhoneAyna.yeni.exe"
Type: filesandordirs; Name: "{app}\bin\uxplay"
Type: dirifempty; Name: "{app}\bin"
Type: dirifempty; Name: "{app}"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';

// Uygulama tek kopya çalışır ve exe'si açıkken değiştirilemez; bildirim alanında gizli durabildiği
// için kullanıcıdan kapatması istenmez, süreç ağacı (AirPlay alıcısı dahil) kapatılır.
procedure CloseApp;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /T /IM iPhoneAyna.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  if Code = 0 then
    Sleep(1000);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  CloseApp;
  Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Cmd: String;
begin
  if CurUninstallStep = usUninstall then
  begin
    CloseApp;
    // Yalnız bu kurulumu başlatan kayıt silinir (başka klasördeki bir kopyanınki kalır).
    if RegQueryStringValue(HKCU, RunKey, 'iPhoneAyna', Cmd) and
       (Pos(Lowercase(ExpandConstant('{app}\iPhoneAyna.exe')), Lowercase(Cmd)) > 0) then
      RegDeleteValue(HKCU, RunKey, 'iPhoneAyna');
    // Sessiz kaldırmada ayarlar (Mac eşleşmesi dahil) korunur; yalnız kullanıcı "Evet" derse silinir.
    if (not UninstallSilent) and (MsgBox(CustomMessage('RemoveSettings'), mbConfirmation, MB_YESNO) = IDYES) then
      RegDeleteKeyIncludingSubkeys(HKCU, 'Software\iPhoneAyna');
  end;
end;
