RSpec.describe Widget do
  include_context "raw http server"
  let(:body) { "x" }

  it { http_response(200) }

  it "serves" do
    serving { |s| s.write(http_response(500)) }
    serving { |s| s.write(body) }
  end
end
