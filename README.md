# Steam Bypass

## Tải về
👉 Tải bản chạy ngay mới nhất tại mục **[Releases](https://github.com/mohoiroi/Bypass-Steam/releases)**:
- Tải trực tiếp file `steam_bypass.exe` hoặc file nén `.zip`.
- Không cần cài đặt, nhấp đúp là chạy ngay.

## Tính năng
- **Chống chặn DNS (DNS-over-HTTPS):** Phân giải IP thật qua Cloudflare (`1.1.1.1` / `1.0.0.1`) và dự phòng Google (`8.8.8.8`), tự thử nhiều IP CDN nếu IP đầu lỗi.
- **Chống chặn SNI/DPI:** Tách gói TLS `ClientHello` (HTTPS) và HTTP tại proxy cục bộ `127.0.0.1:8899`.
- **Đi thẳng CDN tải game:** Tùy chọn giữ nguyên tốc độ tải game gốc từ cache nội mạng VN (bỏ qua proxy cho `steamcontent.com`, `akamaihd.net`) trong khi vẫn bypass Store, Cộng đồng và Đăng nhập.
- **Thêm tên miền web khác (tab *Web khác*):**
  - Nhập hoặc dán **nhiều tên miền cùng lúc** (cách nhau bằng dấu phẩy, khoảng trắng hoặc xuống dòng).
  - Gợi ý 1-click các trang hay bị chặn DPI (`bbc.com`, `medium.com`, `reddit.com`, `imgur.com`, `pixiv.net`).
  - Có công tắc **bật/tắt tạm từng trang** mà không cần xóa, tự lưu cài đặt bền vững.
  - *(Lưu ý: chỉ vượt chặn DNS/DPI nhà mạng; trang tự chặn IP quốc gia/WAF lỗi `403` cần VPN.)*
- **Khởi chạy cùng Windows:** Tự động chạy ẩn dưới khay hệ thống (`--minimized`) khi mở máy.
- **Tự phục hồi kết nối:** Tự xóa cache DNS và kiểm tra lại khi máy thức dậy sau Sleep hoặc khi đổi Wi-Fi/LAN; kèm nút **Khôi phục mạng (Gỡ proxy)** ngay trong menu khay hệ thống.
- **Chạy độc lập:** File `.exe` duy nhất (~410 KB, RAM ~9 MB), không cần cài thư viện ngoài hay quyền Admin.

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
