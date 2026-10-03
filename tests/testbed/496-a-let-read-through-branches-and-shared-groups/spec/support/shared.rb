RSpec.shared_examples "a defaulted thing" do
  let(:mode) { defined?(super) ? super() : :none }

  it { expect(mode).to be }
end

RSpec.shared_context "with a server" do
  def serving
    Thread.new { accept_loop }
  end

  def accept_loop
    1
  end
end
