RSpec.describe Billing::Invoice do
  it "totals" do
    Billing::Invoice.new.total
  end
end
