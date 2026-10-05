RSpec.describe "Widget" do
  def with_scheduler
    scheduler_class.new
    yield
  end

  module WidgetAssertions
    def self.included(group)
      group.class_eval do
        it "runs" do
          with_scheduler { expect(1).to eq(1) }
        end
      end
    end
  end

  describe "with the toy scheduler" do
    let(:scheduler_class) { Object }
    include WidgetAssertions
  end

  describe "with a plain helper" do
    let(:unused_class) { Object }
    include Comparable
  end
end
