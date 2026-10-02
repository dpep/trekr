module Api
  module Concerns
    module TokenAuth
      def skip_auth? = false

      def audited? = true

      def unused_check? = false
    end
  end
end
