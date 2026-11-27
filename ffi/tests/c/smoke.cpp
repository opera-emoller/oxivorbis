// C++ smoke test: the header compiles cleanly as C++ (-Wall -Wextra
// -Werror -pedantic) and links. Built and run by ffi/tests/c_api.rs.
#include "oxivorbis.h"

#include <cstdio>
#include <cstring>
#include <memory>
#include <vector>

namespace {

struct OptionsDeleter {
    void operator()(oxv_options *o) const { oxv_options_free(o); }
};
struct EncoderDeleter {
    void operator()(oxv_encoder *e) const { oxv_encoder_free(e); }
};

}  // namespace

int main() {
    std::unique_ptr<oxv_options, OptionsDeleter> opts(oxv_options_new());
    if (!opts || oxv_options_set_quality(opts.get(), 3.0f) != OXV_STATUS_OK) {
        return 1;
    }
    oxv_encoder *raw = nullptr;
    oxv_status st = oxv_encoder_new(48000, 1, opts.get(), &raw);
    std::unique_ptr<oxv_encoder, EncoderDeleter> enc(raw);
    if (st != OXV_STATUS_OK) {
        std::fprintf(stderr, "oxv_encoder_new: %s\n", oxv_status_message(st));
        return 1;
    }
    std::vector<float> pcm(4800, 0.25f);
    std::vector<uint8_t> ogg;
    const uint8_t *p = nullptr;
    size_t n = 0;
    if (oxv_encoder_write_f32(enc.get(), pcm.data(), pcm.size(), &p, &n) != OXV_STATUS_OK) {
        return 1;
    }
    ogg.insert(ogg.end(), p, p + n);
    if (oxv_encoder_finish(enc.get(), &p, &n) != OXV_STATUS_OK) {
        return 1;
    }
    ogg.insert(ogg.end(), p, p + n);
    if (ogg.size() < 4 || std::memcmp(ogg.data(), "OggS", 4) != 0) {
        return 1;
    }
    std::printf("C++ smoke ok: %zu bytes, oxivorbis %s\n", ogg.size(), oxv_version());
    return 0;
}
