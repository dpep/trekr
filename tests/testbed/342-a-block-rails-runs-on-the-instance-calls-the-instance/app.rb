module Concern
end

class Base
  def self.rescue_from(*classes, with: nil, &block); end
  def self.before_action(*names, **options, &block); end
end

module Guarded
  extend Concern

  included do
    rescue_from ArgumentError do |error|
      respond_error(error)
    end
  end

  class_methods do
    def guarded_action(*names, **options)
      before_action(*names, **options.merge(if: [-> { !uses_token? }]))
    end
  end

  def uses_token?
    true
  end

  def respond_error(error); end
end

class WidgetsController < Base
  include Guarded

  guarded_action :fetch_widget
  before_action :fetch_widget, unless: -> { skipped? }
  rescue_from KeyError do
    respond_missing
  end

  def self.filtered
    before_action :fetch_widget, if: -> { allowed? }
  end

  def fetch_widget; end

  private

  def skipped?; end
  def allowed?; end
  def respond_missing; end
end
