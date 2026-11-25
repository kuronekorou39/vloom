import 'package:flutter_test/flutter_test.dart';
import 'package:vloom/update_service.dart';

Map<String, dynamic> release(String tag, {bool apk = true, bool pre = false}) => {
      'tag_name': tag,
      'prerelease': pre,
      'body': 'notes',
      'assets': [
        if (apk) {'name': 'app-release.apk', 'browser_download_url': 'https://x/$tag.apk'},
      ],
    };

void main() {
  test('版の比較', () {
    expect(UpdateService.compareVersions('0.5.0', '0.4.1'), greaterThan(0));
    expect(UpdateService.compareVersions('0.4.10', '0.4.9'), greaterThan(0));
    expect(UpdateService.compareVersions('0.4.1', '0.4.1'), 0);
    expect(UpdateService.compareVersions('1.0', '1.0.0'), 0);
  });

  test('新しくて APK の付いた正式版だけを勧める', () {
    expect(UpdateService.pickUpdate(release('v0.5.0'), '0.4.1')?.apkUrl, 'https://x/v0.5.0.apk');
    expect(UpdateService.pickUpdate(release('v0.4.1'), '0.4.1'), isNull);
    expect(UpdateService.pickUpdate(release('v0.3.0'), '0.4.1'), isNull);
    expect(UpdateService.pickUpdate(release('v0.5.0', apk: false), '0.4.1'), isNull);
    expect(UpdateService.pickUpdate(release('v0.5.0', pre: true), '0.4.1'), isNull);
  });
}
