module Api
  extend ActiveSupport::Concern

  module ClassMethods
    def build
    end
  end

  included do
    build
  end
end

module Admin
  extend ActiveSupport::Concern
  include Api
end

module Plain
  include Api
end

class Endpoint
  include Api
end

Api.build
Admin.build
Plain.build
Endpoint.build
