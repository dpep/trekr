module Callbacks
  extend ActiveSupport::Concern

  class_methods do
    [:before, :after].each do |phase|
      define_method "#{phase}_step" do |*names|
      end
    end

    NAMES.each do |phase|
      define_method "#{phase}_hook" do
      end
    end
  end
end

class Widget
  include Callbacks

  before_step :prepare
  after_hook :cleanup
end
