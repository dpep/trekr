module Trackable
  extend ActiveSupport::Concern

  included do
    define_model_callbacks :save, :destroy
    define_model_callbacks :touch, only: :after
  end
end

class Widget
  include Trackable

  before_save :prepare
  after_touch :note
  before_touch :never
end
