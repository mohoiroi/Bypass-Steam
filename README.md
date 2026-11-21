# Steam Bypass

## Tải về
Xem phiên bản mới nhất ở mục **[Releases](https://github.com/mohoiroi/Bypass-Steam/releases)**.
Nếu Release có tệp `steam_bypass.exe` hoặc `steam_bypass-windows-x64.zip` trong Assets, có thể tải bản chạy cho Windows từ đó. Mỗi Release cũng có bản lưu mã nguồn do GitHub tạo.

## Tính năng
- **Tùy chọn máy chủ DNS-over-HTTPS (DoH):** Chọn Cloudflare, Google, AdGuard hoặc nhập một URL HTTPS trong mục **Custom**. Chọn **Thử kết nối** để kiểm tra địa chỉ trước khi lưu.
- **Đo Ping / Độ trễ kết nối (Latency ms):** Đo thời gian bắt tay TCP trực tiếp, hiển thị độ trễ trung bình và từng cụm dịch vụ / máy chủ / web tùy chỉnh theo thang màu trực quan (`<60ms`, `60–150ms`, `>150ms`).
- **Chống chặn SNI/DPI:** Tách gói TLS `ClientHello` (HTTPS) và HTTP tại proxy cục bộ `127.0.0.1:8899`.
- **Đi thẳng CDN tải game:** Trong tab **Tiện ích**, bật tùy chọn này để tận dụng cache nội mạng khi tải game (bỏ qua proxy cho `steamcontent.com`, `akamaihd.net`) trong khi vẫn bypass Store, Cộng đồng và Đăng nhập.
- **Thêm tên miền web khác (tab *Web khác*):**
  - Nhập hoặc dán **nhiều tên miền cùng lúc** (cách nhau bằng dấu phẩy, khoảng trắng hoặc xuống dòng).
  - Có công tắc **bật/tắt tạm từng trang** mà không cần xóa, tự lưu cài đặt bền vững.
  - *(Lưu ý: chỉ vượt chặn DNS/DPI nhà mạng; trang tự chặn IP quốc gia/WAF lỗi `403` cần VPN.)*
- **Tiện ích:** Chọn giao diện sáng/tối, kiểm tra cập nhật và bật khởi chạy cùng Windows. Khi bật khởi chạy, ứng dụng tự động chạy ẩn dưới khay hệ thống (`--minimized`) khi mở máy.
- **Sao lưu cấu hình:** Trong **Tiện ích**, dùng **Xuất tệp** hoặc **Nhập tệp** với bản sao lưu `.sbconf`. Tệp lưu danh sách tên miền và trạng thái từng mục, DoH, giao diện và tùy chọn CDN. Xác nhận nhập sẽ thay thế các mục này.
- **Khôi phục proxy sau sự cố:** Lần mở tiếp theo sẽ phát hiện cấu hình proxy tự động còn sót, khôi phục địa chỉ proxy trước đó của Windows nếu có và để proxy Steam Bypass ở trạng thái tắt.
- **Tự phục hồi kết nối:** Tự xóa cache DNS và kiểm tra lại khi máy thức dậy sau Sleep hoặc khi đổi Wi-Fi/LAN; kèm nút **Khôi phục mạng (Gỡ proxy)** ngay trong menu khay hệ thống.
- **Điều khiển nhanh (Win32/GDI+):** Cột trái có trạng thái kết nối, công tắc bật/tắt proxy, độ trễ, số máy chủ phản hồi và bộ chọn DoH. Chuyển giữa Steam, Web khác và Tiện ích ở cột phải.

## Cách sử dụng
1. Chạy `steam_bypass.exe` (không cần quyền Admin).
2. Khi các nhóm dịch vụ chuyển xanh **Đã bypass**, mở hoặc tải lại Steam (Store, Community, tải game...).
3. Bấm **Thoát** ở góc dưới bên phải hoặc chọn **Thoát** từ menu khay hệ thống để dừng ứng dụng và gỡ proxy.
4. Nếu app thoát bất thường, mở lại app để tự khôi phục proxy. Bạn cũng có thể khôi phục bằng lệnh:
   ```powershell
   steam_bypass.exe off
   ```


## Giấy phép
MIT License — xem [LICENSE](LICENSE).
