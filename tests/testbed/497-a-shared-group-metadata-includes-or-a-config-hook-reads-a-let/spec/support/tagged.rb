RSpec.shared_context "with a tagged gadget", :gadget do
  let(:gadget) { build(gadget_name) }
end

RSpec.configure do |config|
  config.before(:each) { log_in(current_owner) }
  config.before(:suite) { start(suite_only) }
end
