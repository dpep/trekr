module Serializing
  def serialize
    super
  end
end

module Auditing
  def audit
  end
end

module Api
  extend ActiveSupport::Concern

  included do
    include Auditing
    prepend Serializing
  end

  def serialize
  end
end

module Admin
  extend ActiveSupport::Concern
  include Api
end

class Endpoint
  include Api

  def serialize
  end
end

class Console
  include Admin
end

Endpoint.new.audit
Endpoint.new.serialize
Console.new.serialize
