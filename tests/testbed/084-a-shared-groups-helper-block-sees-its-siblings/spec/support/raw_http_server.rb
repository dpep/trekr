RSpec.shared_context "raw http server" do
  let(:port) { 1 }

  def serving(&handler)
    handler.call(:socket)
  end

  def http_response(status)
    status
  end
end
