# =============================================================
#  SystemControl.ps1 - Windows 系统控制面板
#  功能: 音量 / 亮度 / WiFi(扫描+连接) 自绘界面, 蓝牙跳转系统设置
#  运行: powershell -NoProfile -ExecutionPolicy Bypass -STA -File SystemControl.ps1
# =============================================================

Add-Type -AssemblyName PresentationFramework

$script:suppressEvents = $true
$script:wifiNetworks = @()

# ---------- C#: 核心音频接口 (IAudioEndpointVolume) ----------
$csSource = @'
using System;
using System.Runtime.InteropServices;

namespace WinAudio {
    [Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IAudioEndpointVolume {
        int RegisterControlChangeNotify(IntPtr pNotify);
        int UnregisterControlChangeNotify(IntPtr pNotify);
        int GetChannelCount(out uint pnChannelCount);
        int SetMasterVolumeLevel(float fLevelDB, Guid pgEventContext);
        int SetMasterVolumeLevelScalar(float fLevel, Guid pgEventContext);
        int GetMasterVolumeLevel(out float pfLevelDB);
        int GetMasterVolumeLevelScalar(out float pfLevel);
        int SetMute(bool bMute, Guid pgEventContext);
        int GetMute(out bool pbMute);
        int GetVolumeStepInfo(out uint pnStep, out uint pnStepCount);
        int VolumeStepUp(Guid pgEventContext);
        int VolumeStepDown(Guid pgEventContext);
        int QueryHardwareSupport(out uint pdwHardwareSupportMask);
        int GetVolumeRange(out float pfMin, out float pfMax, out float pfIncrement);
    }

    [Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IMMDeviceEnumerator {
        int EnumAudioEndpoints(uint dataFlow, uint dwStateMask, out IntPtr ppDevices);
        int GetDefaultAudioEndpoint(uint dataFlow, uint role, out IMMDevice ppEndpoint);
        int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string pwstrId, out IMMDevice ppDevice);
        int RegisterEndpointNotificationCallback(IntPtr pClient);
        int UnregisterEndpointNotificationCallback(IntPtr pClient);
    }

    [Guid("D666063F-158E-4E82-81AE-285E36566AEB"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IMMDevice {
        int Activate(ref Guid iid, uint dwClsCtx, IntPtr pActivationParams, [MarshalAs(UnmanagedType.IUnknown)] out object ppInterface);
        int OpenPropertyStore(uint stgmAccess, out IntPtr ppProperties);
        int GetId([MarshalAs(UnmanagedType.LPWStr)] out string ppstrId);
        int GetState(out uint pdwState);
    }

    public static class AudioEndpoint {
        static IAudioEndpointVolume GetEndpoint() {
            Guid clsid = new Guid("BCDE0395-E52F-467C-8E3D-C4579291692E");
            object obj = Activator.CreateInstance(Type.GetTypeFromCLSID(clsid));
            IMMDeviceEnumerator enumerator = (IMMDeviceEnumerator)obj;
            IMMDevice device;
            int hr = enumerator.GetDefaultAudioEndpoint(0, 1, out device); // eRender, eConsole
            Marshal.ThrowExceptionForHR(hr);
            Guid iid = typeof(IAudioEndpointVolume).GUID;
            object endpoint;
            hr = device.Activate(ref iid, 1, IntPtr.Zero, out endpoint);
            Marshal.ThrowExceptionForHR(hr);
            return (IAudioEndpointVolume)endpoint;
        }

        public static double GetVolume() {
            float level;
            Marshal.ThrowExceptionForHR(GetEndpoint().GetMasterVolumeLevelScalar(out level));
            return level * 100.0;
        }

        public static void SetVolume(double percent) {
            if (percent < 0) { percent = 0; }
            if (percent > 100) { percent = 100; }
            Marshal.ThrowExceptionForHR(GetEndpoint().SetMasterVolumeLevelScalar((float)(percent / 100.0), Guid.Empty));
        }

        public static bool GetMute() {
            bool muted;
            Marshal.ThrowExceptionForHR(GetEndpoint().GetMute(out muted));
            return muted;
        }

        public static void SetMute(bool muted) {
            Marshal.ThrowExceptionForHR(GetEndpoint().SetMute(muted, Guid.Empty));
        }
    }
}
'@
Add-Type -TypeDefinition $csSource

# ---------- 音量 ----------
function Get-VolumeLevel {
    try { return [WinAudio.AudioEndpoint]::GetVolume() } catch { return -1 }
}
function Set-VolumeLevel([double]$percent) {
    try { [WinAudio.AudioEndpoint]::SetVolume($percent); return $true } catch { return $false }
}
function Get-MuteState {
    try { return [WinAudio.AudioEndpoint]::GetMute() } catch { return $false }
}
function Set-MuteState([bool]$muted) {
    try { [WinAudio.AudioEndpoint]::SetMute($muted) } catch { }
}

# ---------- 亮度 ----------
function Get-BrightnessLevel {
    try {
        $b = Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightness -ErrorAction Stop |
             Select-Object -First 1
        return [int]$b.CurrentBrightness
    } catch { return -1 }
}
function Set-BrightnessLevel([int]$percent) {
    try {
        if ($percent -lt 0) { $percent = 0 }
        if ($percent -gt 100) { $percent = 100 }
        $m = Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods -ErrorAction Stop |
             Select-Object -First 1
        Invoke-CimMethod -InputObject $m -MethodName WmiSetBrightness `
            -Arguments @{ Timeout = 0; Brightness = [byte]$percent } | Out-Null
        return $true
    } catch { return $false }
}

# ---------- WiFi ----------
function Get-WifiNetworks {
    $raw = & netsh.exe wlan show networks mode=bssid 2>$null
    $nets = [ordered]@{}
    $current = $null
    foreach ($line in $raw) {
        if ($line -match '^\s*SSID\s+\d+\s*:\s*(.+?)\s*$') {
            $current = $Matches[1]
            if (-not $nets.Contains($current)) {
                $nets[$current] = [pscustomobject]@{ Ssid = $current; Signal = 0; Auth = '' }
            }
        }
        elseif ($current) {
            if ($line -match '^\s*(?:信号|Signal)\s*:\s*(\d+)') {
                $nets[$current].Signal = [int]$Matches[1]
            }
            elseif ($line -match '^\s*(?:身份验证|Authentication)\s*:\s*(.+?)\s*$') {
                $nets[$current].Auth = $Matches[1]
            }
        }
    }
    return @($nets.Values | Sort-Object -Property Signal -Descending)
}

function Get-WifiProfiles {
    $raw = & netsh.exe wlan show profiles 2>$null
    $list = @()
    foreach ($line in $raw) {
        if ($line -match '(?:所有用户配置文件|All User Profile)\s*:\s*(.+?)\s*$') {
            $list += $Matches[1]
        }
    }
    return $list
}

function Get-WifiStatus {
    $raw = & netsh.exe wlan show interfaces 2>$null
    $state = ''; $ssid = ''
    foreach ($line in $raw) {
        if ($line -match '^\s*(?:状态|State)\s*:\s*(.+?)\s*$') { $state = $Matches[1] }
        elseif ($line -match '^\s*SSID\s*:\s*(.+?)\s*$') { $ssid = $Matches[1] }
    }
    if ($ssid) { return "已连接: $ssid  ($state)" }
    return "未连接无线网络"
}

function New-WifiProfileXml([string]$ssid, [string]$auth, [string]$password) {
    $esc = [System.Security.SecurityElement]::Escape($ssid)
    $authType = 'WPA2PSK'; $encType = 'AES'
    if ($auth -match 'Open|开放') { $authType = 'open'; $encType = 'none' }
    elseif ($auth -match 'WPA3')  { $authType = 'WPA3SAE' }
    elseif ($auth -match 'WPA(?!2|3)') { $authType = 'WPAPSK' }
    $keyXml = ''
    if ($authType -ne 'open') {
        $escKey = [System.Security.SecurityElement]::Escape($password)
        $keyXml = "<sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>$escKey</keyMaterial></sharedKey>"
    }
    return @"
<?xml version="1.0"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1">
  <name>$esc</name>
  <SSIDConfig><SSID><name>$esc</name></SSID></SSIDConfig>
  <connectionType>ESS</connectionType>
  <connectionMode>auto</connectionMode>
  <MSM><security>
    <authEncryption><authentication>$authType</authentication><encryption>$encType</encryption><useOneX>false</useOneX></authEncryption>
    $keyXml
  </security></MSM>
</WLANProfile>
"@
}

function Show-PasswordDialog([string]$ssid) {
    $script:dlgResult = $null
    $win = New-Object System.Windows.Window
    $win.Title = '连接 WiFi'
    $win.Width = 340; $win.Height = 180
    $win.WindowStartupLocation = 'CenterScreen'
    $win.ResizeMode = 'NoResize'
    $sp = New-Object System.Windows.Controls.StackPanel
    $sp.Margin = '14'
    $tb = New-Object System.Windows.Controls.TextBlock
    $tb.Text = "输入网络 [$ssid] 的密码:"
    $tb.Margin = '0,0,0,8'; $tb.TextWrapping = 'Wrap'
    $pb = New-Object System.Windows.Controls.PasswordBox
    $pb.Margin = '0,0,0,12'
    $btn = New-Object System.Windows.Controls.Button
    $btn.Content = '连接'; $btn.Padding = '20,4'
    $btn.HorizontalAlignment = 'Right'
    $sp.Children.Add($tb) | Out-Null
    $sp.Children.Add($pb) | Out-Null
    $sp.Children.Add($btn) | Out-Null
    $win.Content = $sp
    $script:dlgPb = $pb
    $script:dlgWin = $win
    $btn.Add_Click({
        $script:dlgResult = $script:dlgPb.Password
        $script:dlgWin.Close()
    })
    $win.ShowDialog() | Out-Null
    return $script:dlgResult
}

function Connect-WifiNetwork([string]$ssid, [string]$auth) {
    $profiles = Get-WifiProfiles
    if ($profiles -contains $ssid) {
        & netsh.exe wlan connect name=$ssid ssid=$ssid | Out-Null
        Start-Sleep -Seconds 2
        return (Get-WifiStatus)
    }
    if ($auth -match 'Open|开放') {
        $xml = New-WifiProfileXml -ssid $ssid -auth $auth -password ''
    } else {
        $pwd = Show-PasswordDialog -ssid $ssid
        if ([string]::IsNullOrEmpty($pwd)) { return "已取消连接 $ssid" }
        $xml = New-WifiProfileXml -ssid $ssid -auth $auth -password $pwd
    }
    $safeName = $ssid -replace '[^\w\-]', '_'
    $tmp = Join-Path $env:TEMP ("wifi_" + $safeName + ".xml")
    [IO.File]::WriteAllText($tmp, $xml, (New-Object System.Text.UTF8Encoding $false))
    & netsh.exe wlan add profile filename=$tmp user=current | Out-Null
    & netsh.exe wlan connect name=$ssid ssid=$ssid | Out-Null
    Start-Sleep -Seconds 2
    return (Get-WifiStatus)
}

function Update-WifiList($lst) {
    $script:wifiNetworks = @()
    $lst.Items.Clear()
    $nets = @()
    try { $nets = Get-WifiNetworks } catch { }
    if ($nets.Count -eq 0) {
        $lst.Items.Add('(未发现网络, 请确认无线网卡已开启)') | Out-Null
        return
    }
    foreach ($n in $nets) {
        $script:wifiNetworks += $n
        $name = $n.Ssid
        if ($name.Length -gt 24) { $name = $name.Substring(0, 24) + '...' }
        $lst.Items.Add(('{0,-26} {1,3}%   {2}' -f $name, $n.Signal, $n.Auth)) | Out-Null
    }
}

# ---------- 主窗口 XAML ----------
$xaml = @'
<Window xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
        xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
        Title="系统控制面板" Height="640" Width="500"
        WindowStartupLocation="CenterScreen" ResizeMode="CanMinimize" Background="#202020">
  <Window.Resources>
    <Style TargetType="TextBlock">
      <Setter Property="Foreground" Value="#E0E0E0"/>
      <Setter Property="FontSize" Value="13"/>
    </Style>
    <Style TargetType="Button">
      <Setter Property="Padding" Value="14,5"/>
      <Setter Property="Margin" Value="0,4,8,0"/>
      <Setter Property="Background" Value="#3A3A3A"/>
      <Setter Property="Foreground" Value="#E0E0E0"/>
      <Setter Property="BorderBrush" Value="#555555"/>
    </Style>
    <Style TargetType="CheckBox">
      <Setter Property="Foreground" Value="#E0E0E0"/>
    </Style>
  </Window.Resources>
  <ScrollViewer VerticalScrollBarVisibility="Auto">
    <StackPanel Margin="18,14">

      <TextBlock Text="音量" FontSize="15" FontWeight="Bold" Margin="0,4,0,6"/>
      <Grid>
        <Grid.ColumnDefinitions>
          <ColumnDefinition Width="*"/>
          <ColumnDefinition Width="56"/>
          <ColumnDefinition Width="Auto"/>
        </Grid.ColumnDefinitions>
        <Slider x:Name="sldVolume" Grid.Column="0" Minimum="0" Maximum="100"
                TickFrequency="1" IsSnapToTickEnabled="True" VerticalAlignment="Center"/>
        <TextBlock x:Name="txtVolume" Grid.Column="1" Text="-- %" VerticalAlignment="Center" Margin="8,0,0,0"/>
        <CheckBox x:Name="chkMute" Grid.Column="2" Content="静音" VerticalAlignment="Center" Margin="10,0,0,0"/>
      </Grid>

      <TextBlock Text="亮度" FontSize="15" FontWeight="Bold" Margin="0,18,0,6"/>
      <Grid>
        <Grid.ColumnDefinitions>
          <ColumnDefinition Width="*"/>
          <ColumnDefinition Width="56"/>
        </Grid.ColumnDefinitions>
        <Slider x:Name="sldBright" Grid.Column="0" Minimum="0" Maximum="100"
                TickFrequency="1" IsSnapToTickEnabled="True" VerticalAlignment="Center"/>
        <TextBlock x:Name="txtBright" Grid.Column="1" Text="-- %" VerticalAlignment="Center" Margin="8,0,0,0"/>
      </Grid>
      <TextBlock x:Name="txtBrightNote" Text="" FontSize="11" Foreground="#999999" Margin="0,2,0,0"/>

      <TextBlock Text="WiFi" FontSize="15" FontWeight="Bold" Margin="0,18,0,6"/>
      <StackPanel Orientation="Horizontal">
        <Button x:Name="btnScan" Content="刷新列表"/>
        <Button x:Name="btnConnect" Content="连接选中"/>
        <Button x:Name="btnDisconnect" Content="断开"/>
      </StackPanel>
      <ListBox x:Name="lstWifi" Height="180" Margin="0,6,0,0" Background="#2A2A2A" Foreground="#E0E0E0"/>
      <TextBlock x:Name="txtWifiStatus" Text="WiFi 状态加载中..." Margin="0,6,0,0" Foreground="#9CDCFE"/>

      <TextBlock Text="蓝牙" FontSize="15" FontWeight="Bold" Margin="0,18,0,6"/>
      <StackPanel Orientation="Horizontal">
        <Button x:Name="btnBt" Content="打开蓝牙设置"/>
      </StackPanel>
      <TextBlock Text="跳转到 Windows 设置 › 蓝牙和其他设备, 可进行设备发现与配对"
                 FontSize="11" Foreground="#999999" Margin="0,4,0,0"/>

    </StackPanel>
  </ScrollViewer>
</Window>
'@

$win = [Windows.Markup.XamlReader]::Parse($xaml)

$controls = @{}
foreach ($n in @('sldVolume','txtVolume','chkMute','sldBright','txtBright','txtBrightNote',
                 'lstWifi','btnScan','btnConnect','btnDisconnect','txtWifiStatus','btnBt')) {
    $controls[$n] = $win.FindName($n)
}

# ---------- 事件绑定 ----------
$controls['sldVolume'].Add_ValueChanged({
    param($sender, $e)
    if ($script:suppressEvents) { return }
    $v = [int]$sender.Value
    [void](Set-VolumeLevel $v)
    $controls['txtVolume'].Text = "$v %"
})

$controls['chkMute'].Add_Click({
    if ($script:suppressEvents) { return }
    Set-MuteState ([bool]$controls['chkMute'].IsChecked.Value)
})

$controls['sldBright'].Add_ValueChanged({
    param($sender, $e)
    if ($script:suppressEvents) { return }
    $v = [int]$sender.Value
    if (Set-BrightnessLevel $v) {
        $controls['txtBright'].Text = "$v %"
    }
})

$controls['btnScan'].Add_Click({
    $win.Cursor = 'Wait'
    $controls['txtWifiStatus'].Text = '正在扫描附近网络...'
    Update-WifiList $controls['lstWifi']
    $controls['txtWifiStatus'].Text = Get-WifiStatus
    $win.Cursor = $null
})

$controls['btnConnect'].Add_Click({
    $idx = $controls['lstWifi'].SelectedIndex
    if ($idx -lt 0 -or $idx -ge $script:wifiNetworks.Count) {
        $controls['txtWifiStatus'].Text = '请先在列表中选择一个网络'
        return
    }
    $n = $script:wifiNetworks[$idx]
    $win.Cursor = 'Wait'
    $controls['txtWifiStatus'].Text = ('正在连接 {0} ...' -f $n.Ssid)
    $status = Connect-WifiNetwork -ssid $n.Ssid -auth $n.Auth
    $controls['txtWifiStatus'].Text = $status
    $win.Cursor = $null
})

$controls['btnDisconnect'].Add_Click({
    & netsh.exe wlan disconnect | Out-Null
    Start-Sleep -Seconds 1
    $controls['txtWifiStatus'].Text = Get-WifiStatus
})

$controls['btnBt'].Add_Click({
    try { Start-Process 'ms-settings:bluetooth' } catch { }
})

$win.Add_Loaded({
    $script:suppressEvents = $true

    # 音量初始化
    $vol = Get-VolumeLevel
    if ($vol -ge 0) {
        $controls['sldVolume'].Value = $vol
        $controls['txtVolume'].Text = ('{0} %' -f [int]$vol)
    } else {
        $controls['sldVolume'].IsEnabled = $false
        $controls['txtVolume'].Text = '不可用'
    }
    $controls['chkMute'].IsChecked = Get-MuteState

    # 亮度初始化
    $bri = Get-BrightnessLevel
    if ($bri -ge 0) {
        $controls['sldBright'].Value = $bri
        $controls['txtBright'].Text = ('{0} %' -f $bri)
    } else {
        $controls['sldBright'].IsEnabled = $false
        $controls['txtBright'].Text = '不可用'
        $controls['txtBrightNote'].Text = '未检测到支持亮度调节的显示器(台式机外接屏通常不支持)'
    }

    # WiFi 初始化
    $controls['txtWifiStatus'].Text = Get-WifiStatus
    Update-WifiList $controls['lstWifi']

    $script:suppressEvents = $false
})

[void]$win.ShowDialog()
