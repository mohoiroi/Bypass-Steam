# Steam Bypass

## Tải về
👉 Tải bản chạy ngay mới nhất tại mục **[Releases](https://github.com/mohoiroi/Bypass-Steam/releases)**:
- Tải trực tiếp file `steam_bypass.exe` hoặc file nén `.zip`.
- Không cần cài đặt, nhấp đúp là chạy ngay.

## Tính năng
- **Chống chặn DNS (DNS-over-HTTPS):** Phân giải IP thật qua Cloudflare (`1.1.1.1` / `1.0.0.1`) và dự phòng Google (`8.8.8.8`).
- **Chống chặn SNI/DPI:** Tách gói TLS `ClientHello` tại proxy cục bộ `127.0.0.1:8899`.
- **Chỉ định tuyến lưu lượng cần bypass:** Tự động cấu hình file PAC (`proxy.pac`) trên Windows cho Steam và các tên miền bạn tự thêm, không ảnh hưởng các trang web khác.
- **Thêm tên miền web khác:** Nhập hoặc dán đường dẫn/tên miền (ví dụ `reddit.com`, `x.com`, `medium.com`) ngay trên giao diện để bypass thêm trang web ngoài Steam, tự lưu cài đặt và xóa dễ dàng từng mục.
- **Khởi chạy cùng Windows:** Tùy chọn tự khởi động ẩn dưới khay hệ thống (`--minimized`) khi mở máy (có sẵn trên giao diện và menu khay hệ thống).
- **Giao diện gọn nhẹ & khay hệ thống:**
  - Theo dõi trạng thái bypass theo các nhóm dịch vụ Steam (**Cửa hàng**, **Cộng đồng**, **Đăng nhập & API**, **Tải game (CDN)**, **Hình ảnh & media**) và nhóm **Web tùy chỉnh**.
  - Nhấp vào từng nhóm để xem tên miền chi tiết, trạng thái DNS và IP đang dùng.
  - Đóng cửa sổ (`×`) sẽ thu gọn xuống khay hệ thống; bấm **Thoát** sẽ tự gỡ cấu hình proxy.
- **Chạy độc lập:** File `.exe` duy nhất (~395 KB, RAM ~8,5 MB), không cần cài thêm thư viện ngoài.

## Cách sử dụng
1. Chạy `steam_bypass.exe` (không cần quyền Admin).
2. Khi các nhóm dịch vụ chuyển xanh **Đã bypass**, mở hoặc tải lại Steam (Store, Community, tải game...).
3. Bấm **Thoát** ở góc dưới hoặc chuột phải vào icon ở khay hệ thống → **Thoát** để dừng và tự gỡ proxy.
4. Nếu máy tắt đột ngột và mạng bị kẹt proxy, chạy lệnh:
   ```powershell
   steam_bypass.exe off
   ```


## Giấy phép
MIT License — xem [LICENSE](LICENSE).
