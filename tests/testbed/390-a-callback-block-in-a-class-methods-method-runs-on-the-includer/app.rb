# ActiveSupport is not indexed: its Concern is read
# by the name written.

class Base
  def self.rescue_from(*classes, &block); end
  def self.after_create(*names, &block); end
end

module Rescuing
  extend ActiveSupport::Concern

  module ClassMethods
    def rescue_duplicates(fields:)
      rescue_from ArgumentError do |error|
        respond_server_error unless respond_duplicate(error, fields:)
      end
    end
  end

  def respond_duplicate(error, fields:); end
end

module Limited
  extend ActiveSupport::Concern

  class_methods do
    def limits(name)
      after_create do
        limiter.record(name)
      end
    end
  end

  def limiter; end
end

class WidgetsController < Base
  include Rescuing
  include Limited

  rescue_duplicates fields: [:name]
  limits :create

  def respond_server_error; end
end
